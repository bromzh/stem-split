//! What the user has chosen so far.

use std::path::{Path, PathBuf};

use stem_split_core::{
    batch::{Batch, OutputClash},
    domain::{
        ByteCount, CleanupTargets, EnhancerId, InputFile, InputFileError, InputFiles, ModelId,
        OutputChoice, OutputDir, OutputDirError, OverwritePolicy, Stem, StemSet,
    },
    job::{Cleanup, MixOutput, MixStems, OutputKind},
    model::ModelStore,
    separate::DevicePreference,
};

use crate::{
    settings::{CleanupChoice, MixChoice, Settings},
    view::text::Locale,
};

/// Which of the two buttons started the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A file for every stem.
    Split,
    /// A mix of the ticked stems, and the stems too if they are kept.
    SplitAndMix,
}

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
#[derive(Debug)]
pub(crate) struct Form {
    inputs: Option<InputFiles>,
    output: OutputChoice,
    model: ModelId,
    device: DevicePreference,
    mix: MixChoice,
    cleanup: CleanupChoice,
    problem: Option<Problem>,
    /// Size of the model files that are not on disk yet, if any. Looked up
    /// when the model changes, not on every frame.
    download: Option<ByteCount>,
}

impl Default for Form {
    /// The form of a first launch.
    fn default() -> Self {
        Self::with(&Settings::initial())
    }
}

impl Form {
    /// A form with no file chosen and the remembered choices filled in.
    pub(crate) fn with(settings: &Settings) -> Self {
        Self {
            inputs: None,
            output: settings.output.clone(),
            model: settings.model,
            device: settings.device,
            mix: settings.mix,
            cleanup: settings.cleanup,
            problem: None,
            download: None,
        }
    }

    /// The choices worth remembering, together with the current language.
    pub(crate) fn settings(&self, locale: Locale) -> Settings {
        Settings {
            locale,
            model: self.model,
            device: self.device,
            output: self.output.clone(),
            mix: self.mix,
            cleanup: self.cleanup,
        }
    }

    /// The ticked stems the current model has.
    pub(crate) fn mix_stems(&self) -> StemSet {
        self.mix.stems.within(self.model.layout())
    }

    pub(crate) const fn keeps_stems(&self) -> bool {
        self.mix.keep_stems
    }

    pub(crate) const fn cleanup(&self) -> CleanupChoice {
        self.cleanup
    }

    /// Whether "Split and mix" has something to mix.
    pub(crate) fn can_mix(&self) -> bool {
        !self.mix_stems().is_empty()
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

    pub(crate) const fn toggle_stem(&mut self, stem: Stem) {
        self.mix.stems = self.mix.stems.toggled(stem);
    }

    /// Ticks every stem of the current model.
    pub(crate) fn select_all_stems(&mut self) {
        self.mix.stems = StemSet::all(self.model.layout());
    }

    pub(crate) const fn select_no_stems(&mut self) {
        self.mix.stems = StemSet::none();
    }

    pub(crate) const fn set_keep_stems(&mut self, keep: bool) {
        self.mix.keep_stems = keep;
    }

    /// Turns the cleanup on or off; the download it needs changes with it.
    pub(crate) fn set_cleanup(&mut self, enabled: bool, store: Option<&ModelStore>) {
        self.cleanup.enabled = enabled;
        self.refresh_download(store);
    }

    pub(crate) const fn set_cleanup_stems(&mut self, stems: bool) {
        self.cleanup.stems = stems;
    }

    pub(crate) const fn set_cleanup_mix(&mut self, mix: bool) {
        self.cleanup.mix = mix;
    }

    /// Whether the chosen network can clean a mix of the ticked stems. One
    /// that tells vocals from instruments cannot clean a mix of both, and
    /// the tick for the mix then counts for nothing.
    pub(crate) fn can_clean_mix(&self) -> bool {
        MixOutput::can_clean_mix(self.cleanup.enhancer, self.mix_stems())
    }

    pub(crate) fn set_enhancer(&mut self, enhancer: EnhancerId, store: Option<&ModelStore>) {
        self.cleanup.enhancer = enhancer;
        self.refresh_download(store);
    }

    /// Selects `model` and looks up whether it still has to be downloaded.
    /// Without a store nothing is known, and no download is announced.
    pub(crate) fn set_model(&mut self, model: ModelId, store: Option<&ModelStore>) {
        self.model = model;
        self.refresh_download(store);
    }

    /// Looks the current model and cleanup network up in `store` again, for
    /// instance after a job has downloaded them.
    pub(crate) fn refresh_download(&mut self, store: Option<&ModelStore>) {
        self.download = store
            .map(|store| store.missing_size(self.model, self.cleanup.enhancer()))
            .filter(|&size| size != ByteCount::ZERO);
    }

    /// What the jobs of `mode` produce. `None` for a mix without stems.
    fn output_kind(&self, mode: Mode) -> Option<OutputKind> {
        let enhancer = self.cleanup.enhancer();
        Some(match mode {
            Mode::Split => OutputKind::Stems {
                cleanup: enhancer.filter(|_| self.cleanup.stems),
            },
            Mode::SplitAndMix => {
                let targets = CleanupTargets::from_ticks(
                    self.cleanup.stems,
                    self.cleanup.mix && self.can_clean_mix(),
                );
                let cleanup = enhancer
                    .zip(targets)
                    .map(|(enhancer, targets)| Cleanup { enhancer, targets });
                let stems = MixStems::new(self.mix_stems(), self.model.layout()).ok()?;
                // The tick for the mix counts only where the network can
                // clean it, so the mix is always valid here.
                OutputKind::Mix(MixOutput::new(stems, self.mix.keep_stems, cleanup).ok()?)
            }
        })
    }

    /// The jobs these choices describe for `mode`, once files have been
    /// chosen and, for a mix, a stem ticked; or why they cannot all be run
    /// together.
    pub(crate) fn batch(
        &self,
        mode: Mode,
        overwrite: OverwritePolicy,
    ) -> Option<Result<Batch, OutputClash>> {
        let inputs = self.inputs.as_ref()?;
        Some(Batch::new(
            inputs,
            &self.output,
            self.model,
            self.device,
            overwrite,
            self.output_kind(mode)?,
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
            .batch(Mode::Split, OverwritePolicy::Refuse)
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
            mix: MixChoice {
                stems: StemSet::none().toggled(Stem::Vocals),
                keep_stems: true,
            },
            cleanup: CleanupChoice {
                enabled: true,
                stems: false,
                mix: true,
                enhancer: EnhancerId::ReverbHq,
            },
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
        assert!(form.batch(Mode::Split, OverwritePolicy::Refuse).is_none());
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
            .batch(Mode::Split, OverwritePolicy::Refuse)
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

    fn output_kind_of(form: &Form, mode: Mode) -> Result<OutputKind, Box<dyn Error>> {
        let batch = form
            .batch(mode, OverwritePolicy::Refuse)
            .ok_or("no batch")??;
        let request = batch.requests().next().ok_or("no job")?;
        Ok(request.output_kind())
    }

    #[test]
    fn split_and_mix_needs_a_ticked_stem_of_the_model() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mut form = Form::default();
        form.set_inputs(&[audio(directory.path(), "song.mp3")?]);
        assert!(form.can_mix(), "every stem is ticked at first");

        form.select_no_stems();
        assert!(!form.can_mix());
        assert!(
            form.batch(Mode::SplitAndMix, OverwritePolicy::Refuse)
                .is_none()
        );
        // Splitting does not need a ticked stem.
        assert!(form.batch(Mode::Split, OverwritePolicy::Refuse).is_some());

        // A stem the model does not have does not count.
        form.toggle_stem(Stem::Piano);
        assert!(!form.can_mix());
        form.set_model(ModelId::Htdemucs6s, None);
        assert!(form.can_mix());
        Ok(())
    }

    #[test]
    fn mix_takes_the_ticked_stems_and_the_cleanup() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mut form = Form::default();
        form.set_inputs(&[audio(directory.path(), "song.mp3")?]);
        form.select_no_stems();
        form.toggle_stem(Stem::Drums);
        form.toggle_stem(Stem::Bass);
        form.set_keep_stems(true);
        form.set_cleanup(true, None);
        form.set_cleanup_mix(true);

        let OutputKind::Mix(mix) = output_kind_of(&form, Mode::SplitAndMix)? else {
            return Err("not a mix".into());
        };
        assert_eq!(
            mix.stems()
                .iter(ModelId::Htdemucs.layout())
                .collect::<Vec<_>>(),
            [Stem::Drums, Stem::Bass]
        );
        assert!(mix.keeps_stems());
        assert_eq!(
            mix.cleanup(),
            Some(Cleanup {
                enhancer: EnhancerId::ReverbHq,
                targets: CleanupTargets::Both,
            })
        );

        form.set_cleanup_stems(false);
        let OutputKind::Mix(mix) = output_kind_of(&form, Mode::SplitAndMix)? else {
            return Err("not a mix".into());
        };
        assert_eq!(
            mix.cleanup().map(|cleanup| cleanup.targets),
            Some(CleanupTargets::Mix)
        );

        // With neither ticked there is nothing to clean.
        form.set_cleanup_mix(false);
        let OutputKind::Mix(mix) = output_kind_of(&form, Mode::SplitAndMix)? else {
            return Err("not a mix".into());
        };
        assert_eq!(mix.cleanup(), None);
        Ok(())
    }

    #[test]
    fn split_cleans_the_stems_only_when_they_are_ticked() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mut form = Form::default();
        form.set_inputs(&[audio(directory.path(), "song.mp3")?]);
        assert_eq!(output_kind_of(&form, Mode::Split)?, OutputKind::STEMS);

        form.set_cleanup(true, None);
        form.set_cleanup_mix(true);
        assert_eq!(
            output_kind_of(&form, Mode::Split)?,
            OutputKind::Stems {
                cleanup: Some(EnhancerId::ReverbHq)
            }
        );

        form.set_cleanup_stems(false);
        assert_eq!(output_kind_of(&form, Mode::Split)?, OutputKind::STEMS);
        Ok(())
    }

    #[test]
    fn vocal_network_leaves_a_mix_of_vocals_and_instruments_alone() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mut form = Form::default();
        form.set_inputs(&[audio(directory.path(), "song.mp3")?]);
        form.set_cleanup(true, None);
        form.set_cleanup_mix(true);
        form.set_enhancer(EnhancerId::VocalBleed, None);

        // Every stem is ticked: vocals and instruments together.
        assert!(!form.can_clean_mix());
        let OutputKind::Mix(mix) = output_kind_of(&form, Mode::SplitAndMix)? else {
            return Err("not a mix".into());
        };
        assert_eq!(
            mix.cleanup().map(|cleanup| cleanup.targets),
            Some(CleanupTargets::Stems)
        );

        // Without the vocals the mix can be cleaned of what leaked in.
        form.toggle_stem(Stem::Vocals);
        assert!(form.can_clean_mix());
        let OutputKind::Mix(mix) = output_kind_of(&form, Mode::SplitAndMix)? else {
            return Err("not a mix".into());
        };
        assert_eq!(
            mix.cleanup().map(|cleanup| cleanup.targets),
            Some(CleanupTargets::Both)
        );
        Ok(())
    }

    #[test]
    fn select_all_ticks_the_stems_of_the_model() {
        let mut form = Form::default();
        form.select_no_stems();
        form.set_model(ModelId::Htdemucs6s, None);

        form.select_all_stems();

        assert_eq!(form.mix_stems(), StemSet::all(ModelId::Htdemucs6s.layout()));
    }

    #[test]
    fn cleanup_adds_its_network_to_the_download() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let store = ModelStore::new(directory.path());
        let mut form = Form::default();
        form.set_model(ModelId::Htdemucs, Some(&store));
        let model = ModelId::Htdemucs.spec().download_size().get();

        form.set_cleanup(true, Some(&store));

        let network = EnhancerId::ReverbHq.spec().file().size().get();
        assert_eq!(form.download().map(ByteCount::get), Some(model + network));
        form.set_cleanup(false, Some(&store));
        assert_eq!(form.download().map(ByteCount::get), Some(model));
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
