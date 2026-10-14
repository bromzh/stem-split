//! Several files split with the same settings, some of them at the same time.

use std::{
    collections::HashMap,
    iter,
    num::NonZeroUsize,
    path::{Path, PathBuf},
};

use crate::{
    domain::{self, InputFile, InputFiles, ModelId, OutputChoice, OverwritePolicy},
    job::SeparationRequest,
    separate::{CpuThreads, DevicePreference},
};

/// How many files are split at the same time.
///
/// Measured on the target machine (GTX 1660 SUPER 6 GB, Ryzen 5 2600) with
/// `htdemucs_6s` and a 106 s track. On the graphics card one file takes
/// 10.6 s; two at once take 14.5 s, three 20.1 s, and four, which no longer
/// fit in the card's memory together, 39.7 s. Three gain little over two and
/// leave less memory for the larger models. On the processor two at once
/// take 79–81 s instead of 97 s one after the other, the same whether each
/// has all the cores or half of them; they get half, to leave the machine
/// responsive.
pub const PARALLEL_JOBS: NonZeroUsize = NonZeroUsize::MIN.saturating_add(1);

/// Two of the chosen files would produce stem files with the same names.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{}` and `{}` would be split into files with the same names",
    first.display(),
    second.display()
)]
pub struct OutputClash {
    first: PathBuf,
    second: PathBuf,
}

impl OutputClash {
    #[must_use]
    pub fn first(&self) -> &Path {
        &self.first
    }

    #[must_use]
    pub fn second(&self) -> &Path {
        &self.second
    }
}

/// The jobs for a set of files: never empty, all with the same model and
/// device, and no two writing to the same file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Batch {
    first: SeparationRequest,
    rest: Vec<SeparationRequest>,
    parallel: NonZeroUsize,
}

impl Batch {
    /// A job for every file of `inputs`, with the results going where
    /// `output` says.
    ///
    /// The jobs run [`PARALLEL_JOBS`] at a time, or fewer if there are fewer
    /// files, and share the processor cores between them.
    ///
    /// # Errors
    ///
    /// Returns [`OutputClash`] for the first two files whose results would
    /// have the same names: files with the same name but a different
    /// extension, from one folder or, when the results go to another folder,
    /// from different folders. On Windows names that differ only in letter
    /// case are the same.
    pub fn new(
        inputs: &InputFiles,
        output: &OutputChoice,
        model: ModelId,
        device: DevicePreference,
        overwrite: OverwritePolicy,
    ) -> Result<Self, OutputClash> {
        let parallel = PARALLEL_JOBS.min(inputs.count());
        let threads = CpuThreads::share(parallel);
        let request = |input: &InputFile| {
            SeparationRequest::new(input.clone(), output, model, device, overwrite)
                .with_cpu_threads(threads)
        };

        // Every stem file name is the output folder, the base name and a
        // suffix that is the same for all files: those two decide a clash.
        let mut claimed: HashMap<PathBuf, &InputFile> = HashMap::new();
        for input in inputs.iter() {
            let folder = output.resolve(input);
            let key = domain::comparable(&folder.path().join(input.base_name()));
            if let Some(earlier) = claimed.insert(key, input) {
                return Err(OutputClash {
                    first: earlier.path().to_path_buf(),
                    second: input.path().to_path_buf(),
                });
            }
        }

        Ok(Self {
            first: request(inputs.first()),
            rest: inputs.iter().skip(1).map(request).collect(),
            parallel,
        })
    }

    /// The jobs in the order the files were chosen, which is also the order
    /// they are started in.
    pub fn requests(&self) -> impl Iterator<Item = &SeparationRequest> {
        iter::once(&self.first).chain(&self.rest)
    }

    #[must_use]
    pub const fn count(&self) -> NonZeroUsize {
        NonZeroUsize::MIN.saturating_add(self.rest.len())
    }

    #[must_use]
    pub const fn model(&self) -> ModelId {
        self.first.model()
    }

    /// How many jobs run at the same time.
    #[must_use]
    pub const fn parallel_jobs(&self) -> NonZeroUsize {
        self.parallel
    }

    /// Output files of all the jobs that are already there.
    #[must_use]
    pub fn existing_outputs(&self) -> Vec<PathBuf> {
        self.requests()
            .flat_map(SeparationRequest::existing_outputs)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    use super::*;
    use crate::domain::{OutputDir, Stem};

    fn audio(directory: &Path, name: &str) -> Result<InputFile, Box<dyn Error>> {
        let path = directory.join(name);
        fs::write(&path, b"")?;
        Ok(InputFile::new(path)?)
    }

    fn batch(inputs: &InputFiles, output: &OutputChoice) -> Result<Batch, OutputClash> {
        Batch::new(
            inputs,
            output,
            ModelId::Htdemucs,
            DevicePreference::Auto,
            OverwritePolicy::Refuse,
        )
    }

    #[test]
    fn repeated_files_are_kept_once_in_their_first_place() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let a = audio(directory.path(), "a.mp3")?;
        let b = audio(directory.path(), "b.flac")?;

        let inputs = InputFiles::new([a.clone(), b.clone(), a.clone()]).ok_or("no inputs")?;

        assert_eq!(inputs.count().get(), 2);
        assert_eq!(inputs.iter().collect::<Vec<_>>(), [&a, &b]);
        assert!(InputFiles::new(Vec::new()).is_none());
        Ok(())
    }

    #[test]
    fn every_file_gets_a_job_in_the_chosen_order() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let names = ["one.mp3", "two.flac", "three.wav"];
        let files = names
            .iter()
            .map(|name| audio(directory.path(), name))
            .collect::<Result<Vec<_>, _>>()?;
        let inputs = InputFiles::new(files).ok_or("no inputs")?;

        let batch = batch(&inputs, &OutputChoice::BesideInput)?;

        assert_eq!(batch.count().get(), 3);
        assert_eq!(batch.model(), ModelId::Htdemucs);
        let vocals: Vec<PathBuf> = batch
            .requests()
            .map(|request| request.stem_path(Stem::Vocals))
            .collect();
        assert_eq!(
            vocals,
            ["one", "two", "three"].map(|base| directory.path().join(format!("{base}.vocals.wav")))
        );
        Ok(())
    }

    #[test]
    fn jobs_run_two_at_a_time_and_share_the_cores() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let one = InputFiles::one(audio(directory.path(), "one.mp3")?);
        let three = InputFiles::new([
            audio(directory.path(), "a.mp3")?,
            audio(directory.path(), "b.mp3")?,
            audio(directory.path(), "c.mp3")?,
        ])
        .ok_or("no inputs")?;

        let single = batch(&one, &OutputChoice::BesideInput)?;
        let several = batch(&three, &OutputChoice::BesideInput)?;

        assert_eq!(single.parallel_jobs().get(), 1);
        assert_eq!(single.first.cpu_threads(), CpuThreads::all());
        assert_eq!(several.parallel_jobs(), PARALLEL_JOBS);
        for request in several.requests() {
            assert_eq!(request.cpu_threads(), CpuThreads::share(PARALLEL_JOBS));
        }
        Ok(())
    }

    #[test]
    fn same_name_in_one_folder_clashes() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mp3 = audio(directory.path(), "song.mp3")?;
        let flac = audio(directory.path(), "song.flac")?;
        let inputs = InputFiles::new([mp3.clone(), flac.clone()]).ok_or("no inputs")?;

        let clash = batch(&inputs, &OutputChoice::BesideInput);

        assert_eq!(
            clash,
            Err(OutputClash {
                first: mp3.path().to_path_buf(),
                second: flac.path().to_path_buf(),
            })
        );
        Ok(())
    }

    #[test]
    fn same_name_in_two_folders_clashes_only_in_a_common_output() -> Result<(), Box<dyn Error>> {
        let left = tempfile::tempdir()?;
        let right = tempfile::tempdir()?;
        let stems = tempfile::tempdir()?;
        let inputs = InputFiles::new([
            audio(left.path(), "song.mp3")?,
            audio(right.path(), "song.mp3")?,
        ])
        .ok_or("no inputs")?;

        assert!(batch(&inputs, &OutputChoice::BesideInput).is_ok());
        let common = OutputChoice::Custom(OutputDir::new(stems.path())?);
        assert!(batch(&inputs, &common).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn names_differing_in_case_clash_on_windows() -> Result<(), Box<dyn Error>> {
        let left = tempfile::tempdir()?;
        let right = tempfile::tempdir()?;
        let stems = tempfile::tempdir()?;
        let inputs = InputFiles::new([
            audio(left.path(), "Song.mp3")?,
            audio(right.path(), "song.wav")?,
        ])
        .ok_or("no inputs")?;

        let common = OutputChoice::Custom(OutputDir::new(stems.path())?);
        assert!(batch(&inputs, &common).is_err());
        Ok(())
    }

    #[test]
    fn existing_outputs_of_every_job_are_listed() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let inputs = InputFiles::new([
            audio(directory.path(), "a.mp3")?,
            audio(directory.path(), "b.mp3")?,
        ])
        .ok_or("no inputs")?;
        let a_bass = directory.path().join("a.bass.wav");
        let b_vocals = directory.path().join("b.vocals.wav");
        fs::write(&a_bass, b"old")?;
        fs::write(&b_vocals, b"old")?;

        let batch = batch(&inputs, &OutputChoice::BesideInput)?;

        assert_eq!(batch.existing_outputs(), [a_bass, b_vocals]);
        Ok(())
    }
}
