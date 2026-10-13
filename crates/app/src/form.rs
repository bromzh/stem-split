//! What the user has chosen so far.

use std::path::Path;

use stem_split_core::{
    domain::{
        ByteCount, InputFile, InputFileError, ModelId, OutputChoice, OutputDir, OutputDirError,
        OverwritePolicy,
    },
    job::SeparationRequest,
    model::ModelStore,
    separate::DevicePreference,
};

/// Why the file or folder the user last picked was not accepted.
#[derive(Debug)]
pub(crate) enum Problem {
    Input(InputFileError),
    Output(OutputDirError),
}

/// The choices on the main screen. Every field holds a value that has
/// already been checked; a pick that fails the check leaves the field as it
/// was and is remembered in `problem` to be shown.
#[derive(Debug, Default)]
pub(crate) struct Form {
    input: Option<InputFile>,
    output: OutputChoice,
    model: ModelId,
    device: DevicePreference,
    problem: Option<Problem>,
    /// Size of the model files that are not on disk yet, if any. Looked up
    /// when the model changes, not on every frame.
    download: Option<ByteCount>,
}

impl Form {
    pub(crate) const fn input(&self) -> Option<&InputFile> {
        self.input.as_ref()
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

    pub(crate) fn set_input(&mut self, path: &Path) {
        match InputFile::new(path) {
            Ok(input) => {
                self.input = Some(input);
                self.problem = None;
            }
            Err(error) => self.problem = Some(Problem::Input(error)),
        }
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

    /// The job these choices describe, once an input file has been chosen.
    pub(crate) fn request(&self, overwrite: OverwritePolicy) -> Option<SeparationRequest> {
        let input = self.input.clone()?;
        Some(SeparationRequest::new(
            input,
            &self.output,
            self.model,
            self.device,
            overwrite,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs};

    use stem_split_core::domain::Stem;

    use super::*;

    #[test]
    fn empty_form_describes_no_job() {
        let form = Form::default();

        assert!(form.input().is_none());
        assert_eq!(form.output(), &OutputChoice::BesideInput);
        assert_eq!(form.model(), ModelId::Htdemucs);
        assert_eq!(form.device(), DevicePreference::Auto);
        assert!(form.request(OverwritePolicy::Refuse).is_none());
    }

    #[test]
    fn chosen_file_makes_a_request_next_to_it() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("song.mp3");
        fs::write(&path, b"")?;
        let mut form = Form::default();

        form.set_input(&path);

        assert!(form.problem().is_none());
        let request = form
            .request(OverwritePolicy::Refuse)
            .ok_or("no request for a chosen file")?;
        assert_eq!(request.input().path(), path);
        assert_eq!(
            request.stem_path(Stem::Vocals),
            directory.path().join("song.vocals.wav")
        );
        Ok(())
    }

    #[test]
    fn rejected_file_keeps_the_previous_one_and_is_remembered() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let good = directory.path().join("song.flac");
        let bad = directory.path().join("notes.txt");
        fs::write(&good, b"")?;
        fs::write(&bad, b"")?;
        let mut form = Form::default();
        form.set_input(&good);

        form.set_input(&bad);

        assert_eq!(form.input().map(InputFile::path), Some(good.as_path()));
        assert!(matches!(
            form.problem(),
            Some(Problem::Input(InputFileError::UnsupportedFormat { .. }))
        ));

        // The next accepted pick clears the complaint.
        form.set_input(&good);
        assert!(form.problem().is_none());
        Ok(())
    }

    #[test]
    fn other_folder_receives_the_stems() -> Result<(), Box<dyn Error>> {
        let music = tempfile::tempdir()?;
        let stems = tempfile::tempdir()?;
        let path = music.path().join("song.wav");
        fs::write(&path, b"")?;
        let mut form = Form::default();
        form.set_input(&path);

        form.set_output_folder(stems.path());
        let custom = form
            .request(OverwritePolicy::Refuse)
            .ok_or("no request")?
            .stem_path(Stem::Bass);
        form.set_output_beside_input();
        let beside = form
            .request(OverwritePolicy::Refuse)
            .ok_or("no request")?
            .stem_path(Stem::Bass);

        assert_eq!(custom, stems.path().join("song.bass.wav"));
        assert_eq!(beside, music.path().join("song.bass.wav"));
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
