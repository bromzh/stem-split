//! The application window: which screen is shown and how one leads to the next.

use std::{
    io, mem,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui;
use stem_split_core::{
    domain::OverwritePolicy,
    job::{Progress, SeparationOutput, SeparationRequest},
    model::ModelStore,
};

use crate::{
    dialogs,
    downloads::{Downloads, Polled},
    form::Form,
    settings::Settings,
    timing::Timing,
    view::{
        self, Clock, ConfirmingAction, DoneAction, EditingAction, FailedAction, RunningAction,
        Stopping, text::Locale,
    },
    worker::{Failure, Outcome, Worker},
};

/// How long closing the window waits for a running job to clean up after
/// itself. A job reacts within one model run, a few seconds at most on a
/// CPU; only a stalled download can take longer.
const EXIT_PATIENCE: Duration = Duration::from_secs(10);

/// How often the running screen is redrawn when the job has nothing new to
/// report, so that the clock on it keeps ticking.
const CLOCK_TICK: Duration = Duration::from_millis(200);

/// A job in progress.
#[derive(Debug)]
struct Run {
    form: Form,
    /// Where the results will be, for the "open folder" button afterwards.
    folder: PathBuf,
    worker: Worker,
    progress: Option<Progress>,
    timing: Timing,
    stopping: Stopping,
}

/// A job that produced its files.
#[derive(Debug)]
struct Finished {
    form: Form,
    folder: PathBuf,
    output: SeparationOutput,
    /// How long the job ran, from its start to the last file in place.
    took: Duration,
    /// Why the folder could not be opened, after the user asked for it.
    problem: Option<io::Error>,
}

/// What the window shows. Each screen owns exactly the data it needs, so a
/// widget cannot outlive the state it belongs to: there is no progress bar
/// without a running job, and no "Split" button while one runs.
#[derive(Debug)]
enum Screen {
    Editing(Form),
    /// Results of an earlier run are in the way.
    Confirming {
        form: Form,
        existing: Vec<PathBuf>,
    },
    Running(Run),
    Done(Finished),
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

/// Whether a file dropped on the window is taken as the next one to split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drop {
    Accepted,
    /// A job is running; its input cannot change under it.
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

    /// The screen after a file was dropped on the window: the form with that
    /// file, from wherever a new job can be started.
    fn with_dropped(self, path: &Path) -> Self {
        let mut form = match self {
            Self::Editing(form) | Self::Confirming { form, .. } | Self::Failed { form, .. } => form,
            Self::Done(finished) => finished.form,
            Self::Running(run) => return Self::Running(run),
        };
        form.set_input(path);
        Self::Editing(form)
    }
}

#[derive(Debug)]
pub(crate) struct StemSplitApp {
    locale: Locale,
    /// What the form shows about the model store: whether a model still has
    /// to be downloaded, and how much the downloaded ones take. Jobs open
    /// the store themselves. Absent when the system offers no cache
    /// directory, in which case nothing is shown and the job reports the
    /// problem.
    downloads: Option<Downloads>,
    screen: Screen,
}

impl StemSplitApp {
    /// `input` is a file to start with, as when the application is launched
    /// by opening a file with it.
    pub(crate) fn new(settings: &Settings, input: Option<&Path>) -> Self {
        let downloads = match ModelStore::in_user_cache() {
            Ok(store) => Some(Downloads::new(store)),
            Err(error) => {
                log::warn!("the model cache is not available: {error}");
                None
            }
        };
        let mut form = Form::with(settings);
        form.refresh_download(downloads.as_ref().map(Downloads::store));
        if let Some(path) = input {
            form.set_input(path);
        }
        Self {
            locale: settings.locale,
            downloads,
            screen: Screen::Editing(form),
        }
    }

    fn store(&self) -> Option<&ModelStore> {
        self.downloads.as_ref().map(Downloads::store)
    }

    /// Whether downloaded models are being deleted right now. No job may
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
                        match form.request(OverwritePolicy::Replace) {
                            Some(request) => launch(form, request, ui.ctx()),
                            None => Screen::Editing(form),
                        }
                    }
                    Some(ConfirmingAction::Back) => Screen::Editing(form),
                    None => Screen::Confirming { form, existing },
                }
            }
            Screen::Running(run) => self.follow(ui, run),
            Screen::Done(finished) => show_finished(ui, locale, finished),
            Screen::Failed { form, failure } => match view::failed(ui, locale, &failure) {
                Some(FailedAction::Back) => Screen::Editing(form),
                None => Screen::Failed { form, failure },
            },
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
            EditingAction::PickInput => {
                if let Some(path) = dialogs::pick_audio_file(self.locale, frame) {
                    form.set_input(&path);
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
                let Some(request) = form.request(OverwritePolicy::Refuse) else {
                    return Screen::Editing(form);
                };
                let existing = request.existing_outputs();
                return if existing.is_empty() {
                    launch(form, request, context)
                } else {
                    Screen::Confirming { form, existing }
                };
            }
        }
        Screen::Editing(form)
    }

    /// Takes in what a running job has reported and either moves on to its
    /// result or draws its progress.
    fn follow(&mut self, ui: &mut egui::Ui, mut run: Run) -> Screen {
        let now = Instant::now();
        let polled = run.worker.poll();
        if let Some(report) = polled.progress {
            run.timing.observe(report, now);
            run.progress = Some(report);
        }
        let Some(outcome) = polled.outcome else {
            ui.ctx().request_repaint_after(CLOCK_TICK);
            let clock = Clock {
                elapsed: run.timing.elapsed(now),
                remaining: run.timing.remaining(now),
            };
            match view::running(ui, self.locale, run.progress, clock, run.stopping) {
                Some(RunningAction::Cancel) => {
                    run.worker.cancel();
                    run.stopping = Stopping::Requested;
                }
                None => {}
            }
            return Screen::Running(run);
        };

        // Whatever the outcome, the job may have downloaded the model.
        if let Some(downloads) = &mut self.downloads {
            downloads.refresh();
        }
        let mut form = run.form;
        form.refresh_download(self.store());
        match outcome {
            Outcome::Completed(output) => Screen::Done(Finished {
                form,
                folder: run.folder,
                output,
                took: run.timing.elapsed(now),
                problem: None,
            }),
            Outcome::Cancelled => Screen::Editing(form),
            Outcome::Failed(failure) => Screen::Failed { form, failure },
        }
    }
}

/// Draws the result of a job and applies what the user does with it.
fn show_finished(ui: &mut egui::Ui, locale: Locale, mut finished: Finished) -> Screen {
    let action = view::done(
        ui,
        locale,
        &finished.folder,
        &finished.output,
        finished.took,
        finished.problem.as_ref(),
    );
    match action {
        Some(DoneAction::OpenFolder) => {
            finished.problem = open::that_detached(&finished.folder).err();
            Screen::Done(finished)
        }
        Some(DoneAction::Back) => Screen::Editing(finished.form),
        None => Screen::Done(finished),
    }
}

/// Starts the job and shows its progress, or the reason it could not start.
fn launch(form: Form, request: SeparationRequest, context: &egui::Context) -> Screen {
    let folder = request.output().path().to_path_buf();
    match Worker::start(request, context.clone()) {
        Ok(worker) => Screen::Running(Run {
            form,
            folder,
            worker,
            progress: None,
            timing: Timing::starting_at(Instant::now()),
            stopping: Stopping::No,
        }),
        Err(error) => Screen::Failed {
            form,
            failure: Failure::NotStarted(error),
        },
    }
}

/// What the user is doing with files dragged from outside the window.
struct Dragging {
    /// A file let go of over the window in this frame.
    dropped: Option<PathBuf>,
    /// Whether files are being held over the window.
    hovering: bool,
}

fn dragging(context: &egui::Context) -> Dragging {
    context.input(|input| Dragging {
        // Several files dropped at once: the application splits one at a
        // time, so the first is taken.
        dropped: input
            .raw
            .dropped_files
            .first()
            .map(|file| file.path().to_path_buf()),
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
        if let Some(path) = &dragging.dropped {
            screen = screen.with_dropped(path);
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
            // Give the job the chance to remove its unfinished files.
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
    use std::{error::Error, fs};

    use stem_split_core::domain::InputFile;

    use super::*;

    fn audio_file(directory: &Path, name: &str) -> Result<PathBuf, io::Error> {
        let path = directory.join(name);
        fs::write(&path, b"")?;
        Ok(path)
    }

    fn input_of(screen: &Screen) -> Option<&Path> {
        screen.form().input().map(InputFile::path)
    }

    #[test]
    fn dropped_file_becomes_the_input_of_the_form() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let path = audio_file(directory.path(), "song.flac")?;

        let screen = Screen::default().with_dropped(&path);

        assert!(matches!(screen, Screen::Editing(_)));
        assert_eq!(input_of(&screen), Some(path.as_path()));
        Ok(())
    }

    #[test]
    fn dropped_file_that_is_not_audio_is_reported_on_the_form() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let good = audio_file(directory.path(), "song.flac")?;
        let bad = audio_file(directory.path(), "notes.txt")?;

        let screen = Screen::default().with_dropped(&good).with_dropped(&bad);

        // The file chosen before stays; the complaint is shown next to it.
        assert_eq!(input_of(&screen), Some(good.as_path()));
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
            let next = screen.with_dropped(&path);
            assert!(matches!(next, Screen::Editing(_)));
            assert_eq!(input_of(&next), Some(path.as_path()));
        }
        Ok(())
    }

    #[test]
    fn remembered_choices_reach_the_form() {
        let settings = Settings {
            locale: Locale::Ru,
            ..Settings::initial()
        };

        let app = StemSplitApp::new(&settings, None);

        assert_eq!(app.locale, Locale::Ru);
        assert_eq!(app.screen.form().settings(app.locale), settings);
        assert!(input_of(&app.screen).is_none());
    }
}
