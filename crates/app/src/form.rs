//! What the user has chosen so far.

use std::path::{Path, PathBuf};

use stem_split_core::{
    batch::{Batch, OutputClash},
    domain::{
        ByteCount, InputFile, InputFileError, InputFiles, ModelId, OutputChoice, OutputDir,
        OutputDirError, OverwritePolicy,
    },
    model::ModelStore,
    separate::DevicePreference,
};

use crate::{settings::Settings, view::text::Locale};

/// Why what the user last picked, or asked for, was not accepted.
#[derive(Debug)]
pub(crate) enum Problem {
    /// Files that cannot be split, one error each; never empty.
    Inputs(Vec<InputFileError>),
    Output(OutputDirError),
    /// Two of the files would write files with the same names.
    Clash(OutputClash),
}

/// The choices on the main screen. Every field holds a value that has
/// already been checked; a pick that fails the check leaves the field as it
/// was and is remembered in `problem` to be shown.
#[derive(Debug, Default)]
pub(crate) struct Form {
    inputs: Option<InputFiles>,
    output: OutputChoice,
    model: ModelId,
    device: DevicePreference,
    problem: Option<Problem>,
    /// Size of the model files that are not on disk yet, if any. Looked up
    /// when the model changes, not on every frame.
    download: Option<ByteCount>,
}

impl Form {
    /// A form with no file chosen and the remembered choices filled in.
    pub(crate) fn with(settings: &Settings) -> Self {
        Self {
            output: settings.output.clone(),
            model: settings.model,
            device: settings.device,
            ..Self::default()
        }
    }

    /// The choices worth remembering, together with the current language.
    pub(crate) fn settings(&self, locale: Locale) -> Settings {
        Settings {
            locale,
            model: self.model,
            device: self.device,
            output: self.output.clone(),
        }
    }

    pub(crate) const fn inputs(&self) -> Option<&InputFiles> {
        self.inputs.as_ref()
    }

    pub(crate) const fn output(&self) -> &OutputChoice {
        &self.output
    }

    pub(crate) const fn model(&self) -> ModelId {
        self.model
    }

    pub(crate) const fn device(&self) -> DevicePreference {
        self.device
    }

    pub(crate) const fn problem(&self) -> Option<&Problem> {
        self.problem.as_ref()
    }

    pub(crate) const fn download(&self) -> Option<ByteCount> {
        self.download
    }

    /// Takes `paths` as the files to split, replacing the ones chosen
    /// before. Those that cannot be split are left out and reported; if
    /// none can, the earlier choice stays.
    pub(crate) fn set_inputs(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let mut accepted = Vec::new();
        let mut rejected = Vec::new();
        for path in paths {
            match InputFile::new(path) {
                Ok(input) => accepted.push(input),
                Err(error) => rejected.push(error),
            }
        }
        if let Some(inputs) = InputFiles::new(accepted) {
            self.inputs = Some(inputs);
        }
        self.problem = (!rejected.is_empty()).then_some(Problem::Inputs(rejected));
    }

    pub(crate) fn set_output_folder(&mut self, path: &Path) {
        match OutputDir::new(path) {
            Ok(directory) => {
                self.output = OutputChoice::Custom(directory);
                self.problem = None;
            }
            Err(error) => self.problem = Some(Problem::Output(error)),
        }
    }

    pub(crate) fn set_output_beside_input(&mut self) {
        self.output = OutputChoice::BesideInput;
    }

    pub(crate) const fn set_device(&mut self, device: DevicePreference) {
        self.device = device;
    }

    /// Selects `model` and looks up whether it still has to be downloaded.
    /// Without a store nothing is known, and no download is announced.
    pub(crate) fn set_model(&mut self, model: ModelId, store: Option<&ModelStore>) {
        self.model = model;
        self.refresh_download(store);
    }

    /// Looks the current model up in `store` again, for instance after a job
    /// has downloaded it.
    pub(crate) fn refresh_download(&mut self, store: Option<&ModelStore>) {
        self.download = store
            .filter(|store| !store.is_cached(self.model))
            .map(|_| self.model.spec().download_size());
    }

    /// The jobs these choices describe, once files have been chosen, or why
    /// they cannot all be run together.
    pub(crate) fn batch(&self, overwrite: OverwritePolicy) -> Option<Result<Batch, OutputClash>> {
        let inputs = self.inputs.as_ref()?;
        Some(Batch::new(
            inputs,
            &self.output,
            self.model,
            self.device,
            overwrite,
        ))
    }

    /// Remembers why the chosen files cannot be split together, to show it.
    pub(crate) fn report_clash(&mut self, clash: OutputClash) {
        self.problem = Some(Problem::Clash(clash));
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    use stem_split_core::{domain::Stem, job::SeparationRequest};

    use super::*;

    fn audio(directory: &Path, name: &str) -> Result<PathBuf, Box<dyn Error>> {
        let path = directory.join(name);
        fs::write(&path, b"")?;
        Ok(path)
    }

    fn chosen(form: &Form) -> Vec<&Path> {
        form.inputs()
            .map(|inputs| inputs.iter().map(InputFile::path).collect())
            .unwrap_or_default()
    }

    fn requests(form: &Form) -> Result<Vec<SeparationRequest>, Box<dyn Error>> {
        let batch = form
            .batch(OverwritePolicy::Refuse)
            .ok_or("no batch for chosen files")??;
        Ok(batch.requests().cloned().collect())
    }

    #[test]
    fn form_starts_from_the_remembered_choices_and_reports_them_back() -> Result<(), Box<dyn Error>>
    {
        let directory = tempfile::tempdir()?;
        let settings = Settings {
            locale: Locale::Ru,
            model: ModelId::HtdemucsFt,
            device: DevicePreference::Cpu,
            output: OutputChoice::Custom(OutputDir::new(directory.path())?),
        };

        let form = Form::with(&settings);

        assert!(form.inputs().is_none());
        assert!(form.problem().is_none());
        assert_eq!(form.model(), ModelId::HtdemucsFt);
        assert_eq!(form.device(), DevicePreference::Cpu);
        assert_eq!(form.settings(Locale::Ru), settings);
        Ok(())
    }

    #[test]
    fn empty_form_describes_no_job() {
        let form = Form::default();

        assert!(form.inputs().is_none());
        assert_eq!(form.output(), &OutputChoice::BesideInput);
        assert_eq!(form.model(), ModelId::Htdemucs);
        assert_eq!(form.device(), DevicePreference::Auto);
        assert!(form.batch(OverwritePolicy::Refuse).is_none());
    }

    #[test]
    fn chosen_files_make_one_job_each_next_to_them() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = audio(directory.path(), "song.mp3")?;
        let second = audio(directory.path(), "other.flac")?;
        let mut form = Form::default();

        form.set_inputs(&[first.clone(), second.clone()]);

        assert!(form.problem().is_none());
        assert_eq!(chosen(&form), [first.as_path(), second.as_path()]);
        let vocals: Vec<PathBuf> = requests(&form)?
            .iter()
            .map(|request| request.stem_path(Stem::Vocals))
            .collect();
        assert_eq!(
            vocals,
            [
                directory.path().join("song.vocals.wav"),
                directory.path().join("other.vocals.wav")
            ]
        );
        Ok(())
    }

    #[test]
    fn new_choice_replaces_the_previous_one() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = audio(directory.path(), "a.mp3")?;
        let second = audio(directory.path(), "b.mp3")?;
        let mut form = Form::default();
        form.set_inputs(&[first]);

        form.set_inputs(std::slice::from_ref(&second));

        assert_eq!(chosen(&form), [second.as_path()]);
        Ok(())
    }

    #[test]
    fn files_that_cannot_be_split_are_left_out_and_reported() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let good = audio(directory.path(), "song.flac")?;
        let bad = audio(directory.path(), "notes.txt")?;
        let missing = directory.path().join("gone.mp3");
        let mut form = Form::default();

        form.set_inputs(&[bad.clone(), good.clone(), missing]);

        assert_eq!(chosen(&form), [good.as_path()]);
        assert!(matches!(
            form.problem(),
            Some(Problem::Inputs(errors)) if errors.len() == 2
        ));

        // A choice with nothing usable keeps the earlier one.
        form.set_inputs(&[bad]);
        assert_eq!(chosen(&form), [good.as_path()]);
        assert!(matches!(form.problem(), Some(Problem::Inputs(errors)) if errors.len() == 1));

        // The next accepted pick clears the complaint.
        form.set_inputs(&[good]);
        assert!(form.problem().is_none());
        Ok(())
    }

    #[test]
    fn files_that_would_overwrite_each_other_cannot_make_a_batch() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mut form = Form::default();
        form.set_inputs(&[
            audio(directory.path(), "song.mp3")?,
            audio(directory.path(), "song.flac")?,
        ]);

        let clash = form
            .batch(OverwritePolicy::Refuse)
            .ok_or("no batch")?
            .err()
            .ok_or("the files should clash")?;
        form.report_clash(clash);

        assert!(matches!(form.problem(), Some(Problem::Clash(_))));
        Ok(())
    }

    #[test]
    fn other_folder_receives_the_stems() -> Result<(), Box<dyn Error>> {
        let music = tempfile::tempdir()?;
        let stems = tempfile::tempdir()?;
        let mut form = Form::default();
        form.set_inputs(&[audio(music.path(), "song.wav")?]);

        form.set_output_folder(stems.path());
        let custom = requests(&form)?
            .first()
            .map(|request| request.stem_path(Stem::Bass));
        form.set_output_beside_input();
        let beside = requests(&form)?
            .first()
            .map(|request| request.stem_path(Stem::Bass));

        assert_eq!(custom, Some(stems.path().join("song.bass.wav")));
        assert_eq!(beside, Some(music.path().join("song.bass.wav")));
        Ok(())
    }

    #[test]
    fn missing_folder_is_rejected_and_remembered() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mut form = Form::default();

        form.set_output_folder(&directory.path().join("missing"));

        assert_eq!(form.output(), &OutputChoice::BesideInput);
        assert!(matches!(
            form.problem(),
            Some(Problem::Output(OutputDirError::Inaccessible { .. }))
        ));
        Ok(())
    }

    #[test]
    fn download_is_announced_only_for_models_not_on_disk() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let store = ModelStore::new(directory.path());
        let mut form = Form::default();

        form.set_model(ModelId::Htdemucs6s, Some(&store));
        assert_eq!(form.model(), ModelId::Htdemucs6s);
        assert_eq!(
            form.download(),
            Some(ModelId::Htdemucs6s.spec().download_size())
        );

        // Without a store nothing is known, so nothing is announced.
        form.set_model(ModelId::Htdemucs, None);
        assert_eq!(form.download(), None);
        Ok(())
    }
}
