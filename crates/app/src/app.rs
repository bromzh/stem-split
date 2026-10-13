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
    form::Form,
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

#[derive(Debug)]
pub(crate) struct StemSplitApp {
    locale: Locale,
    /// Only consulted to tell the user that a model still has to be
    /// downloaded; jobs open the store themselves. Absent when the system
    /// offers no cache directory, in which case nothing is announced and the
    /// job reports the problem.
    models: Option<ModelStore>,
    screen: Screen,
}

impl StemSplitApp {
    /// `input` is a file to start with, as when the application is launched
    /// by opening a file with it.
    pub(crate) fn new(locale: Locale, input: Option<&Path>) -> Self {
        let models = match ModelStore::in_user_cache() {
            Ok(store) => Some(store),
            Err(error) => {
                log::warn!("the model cache is not available: {error}");
                None
            }
        };
        let mut form = Form::default();
        form.refresh_download(models.as_ref());
        if let Some(path) = input {
            form.set_input(path);
        }
        Self {
            locale,
            models,
            screen: Screen::Editing(form),
        }
    }

    /// Draws the current screen and returns the one to show next.
    fn step(&self, ui: &mut egui::Ui, frame: &eframe::Frame, screen: Screen) -> Screen {
        let locale = self.locale;
        match screen {
            Screen::Editing(form) => match view::editing(ui, locale, &form) {
                Some(action) => self.edit(form, action, ui.ctx(), frame),
                None => Screen::Editing(form),
            },
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
        &self,
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
            EditingAction::Model(model) => form.set_model(model, self.models.as_ref()),
            EditingAction::Device(device) => form.set_device(device),
            EditingAction::Start => {
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
    fn follow(&self, ui: &mut egui::Ui, mut run: Run) -> Screen {
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
        let mut form = run.form;
        form.refresh_download(self.models.as_ref());
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

impl eframe::App for StemSplitApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let screen = mem::take(&mut self.screen);
        self.screen = egui::CentralPanel::default()
            .show(ui, |ui| self.step(ui, frame, screen))
            .inner;
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
