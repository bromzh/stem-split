//! The application window: which screen is shown and how one leads to the next.

use std::{
    io, mem,
    path::PathBuf,
    time::{Duration, Instant},
};

use eframe::egui;
use stem_split_core::{batch::Batch, domain::OverwritePolicy, model::ModelStore};

use crate::{
    dialogs,
    downloads::{Downloads, Polled},
    form::Form,
    settings::Settings,
    tracker::{End, Results, Tracker},
    view::{
        self, ConfirmingAction, DoneAction, EditingAction, FailedAction, RunningAction, Stopping,
        text::Locale,
    },
    worker::{Channel, Failure, Worker},
};

/// How long closing the window waits for running jobs to clean up after
/// themselves. A job reacts within one model run, a few seconds at most on a
/// CPU; only a stalled download can take longer.
const EXIT_PATIENCE: Duration = Duration::from_secs(10);

/// How often the running screen is redrawn when the jobs have nothing new
/// to report, so that the clocks on it keep ticking.
const CLOCK_TICK: Duration = Duration::from_millis(200);

/// A batch in progress.
#[derive(Debug)]
struct Run {
    form: Form,
    worker: Worker,
    tracker: Tracker,
    stopping: Stopping,
}

/// A batch that ended with something to show.
#[derive(Debug)]
struct Finished {
    form: Form,
    results: Results,
    /// Why a folder could not be opened, after the user asked for it.
    problem: Option<io::Error>,
}

/// What the window shows. Each screen owns exactly the data it needs, so a
/// widget cannot outlive the state it belongs to: there is no progress bar
/// without a running batch, and no "Split" button while one runs.
#[derive(Debug)]
enum Screen {
    Editing(Form),
    /// Results of an earlier run are in the way.
    Confirming {
        form: Form,
        existing: Vec<PathBuf>,
    },
    /// Boxed: the batch carries much more than any other screen.
    Running(Box<Run>),
    Done(Finished),
    /// The batch could not run at all.
    Failed {
        form: Form,
        failure: Failure,
    },
}

impl Default for Screen {
    fn default() -> Self {
        Self::Editing(Form::default())
    }
}

/// Whether files dropped on the window are taken as the next ones to split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drop {
    Accepted,
    /// A batch is running; its files cannot change under it.
    Ignored,
}

impl Screen {
    /// The user's choices, which every screen carries along.
    const fn form(&self) -> &Form {
        match self {
            Self::Editing(form) | Self::Confirming { form, .. } | Self::Failed { form, .. } => form,
            Self::Running(run) => &run.form,
            Self::Done(finished) => &finished.form,
        }
    }

    const fn form_mut(&mut self) -> &mut Form {
        match self {
            Self::Editing(form) | Self::Confirming { form, .. } | Self::Failed { form, .. } => form,
            Self::Running(run) => &mut run.form,
            Self::Done(finished) => &mut finished.form,
        }
    }

    const fn drop_policy(&self) -> Drop {
        match self {
            Self::Editing(_) | Self::Confirming { .. } | Self::Done(_) | Self::Failed { .. } => {
                Drop::Accepted
            }
            Self::Running(_) => Drop::Ignored,
        }
    }

    /// The screen after files were dropped on the window: the form with
    /// those files, from wherever a new batch can be started.
    fn with_dropped(self, paths: &[PathBuf]) -> Self {
        let mut form = match self {
            Self::Editing(form) | Self::Confirming { form, .. } | Self::Failed { form, .. } => form,
            Self::Done(finished) => finished.form,
            Self::Running(run) => return Self::Running(run),
        };
        form.set_inputs(paths);
        Self::Editing(form)
    }
}

#[derive(Debug)]
pub(crate) struct StemSplitApp {
    locale: Locale,
    /// What the form shows about the model store: whether a model still has
    /// to be downloaded, and how much the downloaded ones take. Jobs open
    /// the store themselves. Absent when the system offers no cache
    /// directory, in which case nothing is shown and the batch reports the
    /// problem.
    downloads: Option<Downloads>,
    screen: Screen,
}

impl StemSplitApp {
    /// `inputs` are files to start with, as when the application is
    /// launched by opening files with it.
    pub(crate) fn new(settings: &Settings, inputs: &[PathBuf]) -> Self {
        let downloads = match ModelStore::in_user_cache() {
            Ok(store) => Some(Downloads::new(store)),
            Err(error) => {
                log::warn!("the model cache is not available: {error}");
                None
            }
        };
        let mut form = Form::with(settings);
        form.refresh_download(downloads.as_ref().map(Downloads::store));
        form.set_inputs(inputs);
        Self {
            locale: settings.locale,
            downloads,
            screen: Screen::Editing(form),
        }
    }

    fn store(&self) -> Option<&ModelStore> {
        self.downloads.as_ref().map(Downloads::store)
    }

    /// Whether downloaded models are being deleted right now. No batch may
    /// start meanwhile: a model it found in place could vanish under it.
    fn clearing(&self) -> bool {
        self.downloads.as_ref().is_some_and(Downloads::is_clearing)
    }

    /// Draws the current screen and returns the one to show next.
    fn step(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame, screen: Screen) -> Screen {
        let locale = self.locale;
        match screen {
            Screen::Editing(form) => {
                match view::editing(ui, locale, &form, self.downloads.as_ref()) {
                    Some(action) => self.edit(form, action, ui.ctx(), frame),
                    None => Screen::Editing(form),
                }
            }
            Screen::Confirming { form, existing } => {
                match view::confirming(ui, locale, &existing) {
                    Some(ConfirmingAction::Replace) => {
                        prepare(form, OverwritePolicy::Replace, ui.ctx())
                    }
                    Some(ConfirmingAction::Back) => Screen::Editing(form),
                    None => Screen::Confirming { form, existing },
                }
            }
            Screen::Running(run) => self.follow(ui, run),
            Screen::Done(finished) => show_finished(ui, locale, finished),
            Screen::Failed { form, failure } => {
                let files = form.inputs().map_or(1, |inputs| inputs.count().get());
                match view::failed(ui, locale, files, &failure) {
                    Some(FailedAction::Back) => Screen::Editing(form),
                    None => Screen::Failed { form, failure },
                }
            }
        }
    }

    /// Applies something the user did on the main form.
    fn edit(
        &mut self,
        mut form: Form,
        action: EditingAction,
        context: &egui::Context,
        frame: &eframe::Frame,
    ) -> Screen {
        match action {
            EditingAction::PickInputs => {
                if let Some(paths) = dialogs::pick_audio_files(self.locale, frame) {
                    form.set_inputs(&paths);
                }
            }
            EditingAction::PickOutputFolder => {
                if let Some(path) = dialogs::pick_folder(frame) {
                    form.set_output_folder(&path);
                }
            }
            EditingAction::OutputBesideInput => form.set_output_beside_input(),
            EditingAction::Model(model) => form.set_model(model, self.store()),
            EditingAction::Device(device) => form.set_device(device),
            EditingAction::Language(locale) => self.locale = locale,
            EditingAction::OpenDownloads => {
                if let Some(downloads) = &mut self.downloads {
                    downloads.open_folder();
                }
            }
            EditingAction::ClearDownloads => {
                if let Some(downloads) = &mut self.downloads {
                    downloads.clear(context);
                }
            }
            EditingAction::Start => {
                if self.clearing() {
                    return Screen::Editing(form);
                }
                return prepare(form, OverwritePolicy::Refuse, context);
            }
        }
        Screen::Editing(form)
    }

    /// Takes in what the batch has reported and either moves on to its
    /// results or draws its progress.
    fn follow(&mut self, ui: &mut egui::Ui, mut run: Box<Run>) -> Screen {
        let now = Instant::now();
        let polled = run.worker.poll();
        for event in polled.events {
            run.tracker.apply(event, now);
        }
        match polled.channel {
            Channel::Open => {
                ui.ctx().request_repaint_after(CLOCK_TICK);
                let overview = run.tracker.overview(now);
                match view::running(ui, self.locale, &overview, run.stopping) {
                    Some(RunningAction::Cancel) => {
                        run.worker.cancel();
                        run.stopping = Stopping::Requested;
                    }
                    None => {}
                }
                return Screen::Running(run);
            }
            Channel::Ended | Channel::Lost => {}
        }

        // Whatever the outcome, the batch may have downloaded the model.
        if let Some(downloads) = &mut self.downloads {
            downloads.refresh();
        }
        let mut form = run.form;
        form.refresh_download(self.store());
        match run.tracker.finish(polled.channel, now) {
            End::NotFetched(failure) => Screen::Failed { form, failure },
            End::Cancelled => Screen::Editing(form),
            End::Results(results) => Screen::Done(Finished {
                form,
                results,
                problem: None,
            }),
        }
    }
}

/// Starts the batch the form describes, unless its files clash or results
/// of an earlier run are in the way and `overwrite` does not allow
/// replacing them, in which case the user is asked first.
fn prepare(mut form: Form, overwrite: OverwritePolicy, context: &egui::Context) -> Screen {
    match form.batch(overwrite) {
        None => Screen::Editing(form),
        Some(Err(clash)) => {
            form.report_clash(clash);
            Screen::Editing(form)
        }
        Some(Ok(batch)) => {
            let existing = match overwrite {
                OverwritePolicy::Refuse => batch.existing_outputs(),
                OverwritePolicy::Replace => Vec::new(),
            };
            if existing.is_empty() {
                launch(form, batch, context)
            } else {
                Screen::Confirming { form, existing }
            }
        }
    }
}

/// Draws the results of a batch and applies what the user does with them.
fn show_finished(ui: &mut egui::Ui, locale: Locale, mut finished: Finished) -> Screen {
    let action = view::done(ui, locale, &finished.results, finished.problem.as_ref());
    match action {
        Some(DoneAction::OpenFolder(index)) => {
            if let Some(file) = finished.results.files.get(index) {
                finished.problem = open::that_detached(&file.folder).err();
            }
            Screen::Done(finished)
        }
        Some(DoneAction::Back) => Screen::Editing(finished.form),
        None => Screen::Done(finished),
    }
}

/// Starts the batch and shows its progress, or the reason it could not start.
fn launch(form: Form, batch: Batch, context: &egui::Context) -> Screen {
    let tracker = Tracker::new(&batch, Instant::now());
    match Worker::start(batch, context.clone()) {
        Ok(worker) => Screen::Running(Box::new(Run {
            form,
            worker,
            tracker,
            stopping: Stopping::No,
        })),
        Err(error) => Screen::Failed {
            form,
            failure: Failure::NotStarted(error),
        },
    }
}

/// What the user is doing with files dragged from outside the window.
struct Dragging {
    /// Files let go of over the window in this frame.
    dropped: Vec<PathBuf>,
    /// Whether files are being held over the window.
    hovering: bool,
}

fn dragging(context: &egui::Context) -> Dragging {
    context.input(|input| Dragging {
        dropped: input
            .raw
            .dropped_files
            .iter()
            .map(|file| file.path().to_path_buf())
            .collect(),
        hovering: !input.raw.hovered_files.is_empty(),
    })
}

impl eframe::App for StemSplitApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(downloads) = &mut self.downloads {
            match downloads.poll() {
                // The model chosen on the form may have gone with the rest.
                Polled::ClearingEnded => self
                    .screen
                    .form_mut()
                    .refresh_download(Some(downloads.store())),
                Polled::Quiet => {}
            }
        }
        let dragging = dragging(ui.ctx());
        let mut screen = mem::take(&mut self.screen);
        if !dragging.dropped.is_empty() {
            screen = screen.with_dropped(&dragging.dropped);
        }
        self.screen = egui::CentralPanel::default()
            .show(ui, |ui| self.step(ui, frame, screen))
            .inner;

        match (dragging.hovering, self.screen.drop_policy()) {
            (true, Drop::Accepted) => view::drop_hint(ui.ctx(), self.locale),
            (true, Drop::Ignored) | (false, Drop::Accepted | Drop::Ignored) => {}
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        self.screen.form().settings(self.locale).save(storage);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        match mem::take(&mut self.screen) {
            // Give the jobs the chance to remove their unfinished files.
            Screen::Running(run) => run.worker.stop(EXIT_PATIENCE),
            Screen::Editing(_)
            | Screen::Confirming { .. }
            | Screen::Done(_)
            | Screen::Failed { .. } => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{error::Error, fs, path::Path};

    use stem_split_core::domain::InputFile;

    use super::*;

    fn audio_file(directory: &Path, name: &str) -> Result<PathBuf, io::Error> {
        let path = directory.join(name);
        fs::write(&path, b"")?;
        Ok(path)
    }

    fn inputs_of(screen: &Screen) -> Vec<&Path> {
        screen
            .form()
            .inputs()
            .map(|inputs| inputs.iter().map(InputFile::path).collect())
            .unwrap_or_default()
    }

    #[test]
    fn dropped_files_become_the_inputs_of_the_form() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = audio_file(directory.path(), "song.flac")?;
        let second = audio_file(directory.path(), "other.mp3")?;

        let screen = Screen::default().with_dropped(&[first.clone(), second.clone()]);

        assert!(matches!(screen, Screen::Editing(_)));
        assert_eq!(inputs_of(&screen), [first.as_path(), second.as_path()]);
        Ok(())
    }

    #[test]
    fn dropped_file_that_is_not_audio_is_reported_on_the_form() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let good = audio_file(directory.path(), "song.flac")?;
        let bad = audio_file(directory.path(), "notes.txt")?;

        let screen = Screen::default()
            .with_dropped(std::slice::from_ref(&good))
            .with_dropped(&[bad]);

        // The files chosen before stay; the complaint is shown next to them.
        assert_eq!(inputs_of(&screen), [good.as_path()]);
        assert!(screen.form().problem().is_some());
        Ok(())
    }

    #[test]
    fn drop_on_a_failure_or_a_confirmation_returns_to_the_form() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = audio_file(directory.path(), "next.mp3")?;
        let failed = Screen::Failed {
            form: Form::default(),
            failure: Failure::Crashed,
        };
        let confirming = Screen::Confirming {
            form: Form::default(),
            existing: vec![directory.path().join("old.vocals.wav")],
        };

        for screen in [failed, confirming] {
            assert_eq!(screen.drop_policy(), Drop::Accepted);
            let next = screen.with_dropped(std::slice::from_ref(&path));
            assert!(matches!(next, Screen::Editing(_)));
            assert_eq!(inputs_of(&next), [path.as_path()]);
        }
        Ok(())
    }

    #[test]
    fn remembered_choices_reach_the_form() {
        let settings = Settings {
            locale: Locale::Ru,
            ..Settings::initial()
        };

        let app = StemSplitApp::new(&settings, &[]);

        assert_eq!(app.locale, Locale::Ru);
        assert_eq!(app.screen.form().settings(app.locale), settings);
        assert!(inputs_of(&app.screen).is_empty());
    }

    #[test]
    fn files_from_the_command_line_are_on_the_form() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let first = audio_file(directory.path(), "a.mp3")?;
        let second = audio_file(directory.path(), "b.mp3")?;

        let app = StemSplitApp::new(&Settings::initial(), &[first.clone(), second.clone()]);

        assert_eq!(inputs_of(&app.screen), [first.as_path(), second.as_path()]);
        Ok(())
    }
}
