//! The application window: which screen is shown and how one leads to the next.

use std::{
    io, mem,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use eframe::egui;
use stem_split_core::{batch::Batch, domain::OverwritePolicy, model::ModelStore};

use crate::{
    dialogs,
    downloads::{Downloads, Polled},
    form::{Form, Mode, Source, UrlAction},
    settings::Settings,
    tracker::{End, Outcome, Results, Tracker},
    view::{
        self, ConfirmingAction, DoneAction, EditingAction, FailedAction, RunningAction, Stopping,
        text::Locale,
    },
    worker::{Channel, Event, Failure, Worker},
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
    Settings {
        form: Form,
        tools: crate::tools::Tools,
    },
    /// Results of an earlier run are in the way.
    Confirming {
        form: Form,
        mode: Mode,
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
            Self::Settings { form, .. }
            | Self::Editing(form)
            | Self::Confirming { form, .. }
            | Self::Failed { form, .. } => form,
            Self::Running(run) => &run.form,
            Self::Done(finished) => &finished.form,
        }
    }

    const fn form_mut(&mut self) -> &mut Form {
        match self {
            Self::Settings { form, .. }
            | Self::Editing(form)
            | Self::Confirming { form, .. }
            | Self::Failed { form, .. } => form,
            Self::Running(run) => &mut run.form,
            Self::Done(finished) => &mut finished.form,
        }
    }

    const fn drop_policy(&self) -> Drop {
        match self {
            Self::Editing(_) | Self::Confirming { .. } | Self::Done(_) | Self::Failed { .. } => {
                Drop::Accepted
            }
            Self::Running(_) | Self::Settings { .. } => Drop::Ignored,
        }
    }

    /// The screen after files were dropped on the window: the form with
    /// those files, from wherever a new batch can be started.
    fn with_dropped(self, paths: &[PathBuf]) -> Self {
        let mut form = match self {
            Self::Editing(form) | Self::Confirming { form, .. } | Self::Failed { form, .. } => form,
            Self::Done(finished) => finished.form,
            Self::Running(run) => return Self::Running(run),
            Self::Settings { form, tools } => return Self::Settings { form, tools },
        };
        form.set_inputs(paths);
        Self::Editing(form)
    }
}

#[derive(Debug)]
enum WindowSizing {
    Opening,
    Following,
    PlaylistPanel,
}

#[derive(Debug)]
pub(crate) struct StemSplitApp {
    window_sizing: WindowSizing,
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
        let downloads = match settings.cache_location.store() {
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
            window_sizing: WindowSizing::Opening,
            locale: settings.locale,
            downloads,
            screen: Screen::Editing(form),
        }
    }

    fn store(&self) -> Option<&ModelStore> {
        self.downloads.as_ref().map(Downloads::store)
    }

    fn fit_window(&mut self, context: &egui::Context, required: egui::Vec2) {
        if context.will_discard() {
            return;
        }
        let fallback = context.content_rect().size();
        let (current, limit, expanded) = context.input(|input| {
            let viewport = input.viewport();
            let current = viewport.inner_rect.map_or(fallback, |rect| rect.size());
            let decoration = viewport
                .outer_rect
                .zip(viewport.inner_rect)
                .map_or(egui::vec2(0.0, 32.0), |(outer, inner)| {
                    (outer.size() - inner.size()).max(egui::Vec2::ZERO)
                });
            let limit = viewport
                .monitor_size
                .map_or(required.max(current), |monitor| {
                    (monitor - decoration - egui::vec2(24.0, 64.0)).max(egui::Vec2::splat(1.0))
                });
            (
                current,
                limit,
                viewport.maximized == Some(true) || viewport.fullscreen == Some(true),
            )
        });
        let minimum = required.min(limit).floor();
        context.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(minimum));
        let target = match self.window_sizing {
            WindowSizing::Opening => minimum,
            WindowSizing::PlaylistPanel => {
                egui::vec2(minimum.x, current.y.max(minimum.y).min(limit.y))
            }
            WindowSizing::Following => current.max(minimum).min(limit),
        };
        if !expanded && (target - current).length() > 0.5 {
            context.send_viewport_cmd(egui::ViewportCommand::InnerSize(target));
        }
        self.window_sizing = WindowSizing::Following;
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
            Screen::Settings { form, tools } => self.show_settings(ui, frame, form, tools),
            Screen::Editing(mut form) => {
                form.poll_preview(ui.ctx(), !self.clearing());
                if let Some(usage) = form.take_preview_usage()
                    && let Some(downloads) = &mut self.downloads
                {
                    downloads.record_usage(usage);
                }
                let editing = view::editing(ui, locale, &form, self.downloads.as_ref());
                self.fit_window(ui.ctx(), editing.minimum);
                match editing.action {
                    Some(action) => self.edit(form, action, ui.ctx(), frame),
                    None => Screen::Editing(form),
                }
            }
            Screen::Confirming {
                form,
                mode,
                existing,
            } => match view::confirming(ui, locale, &existing) {
                Some(ConfirmingAction::Replace) => {
                    prepare_batch(form, mode, OverwritePolicy::Replace, ui.ctx())
                }
                Some(ConfirmingAction::Back) => Screen::Editing(form),
                None => Screen::Confirming {
                    form,
                    mode,
                    existing,
                },
            },
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

    fn change_cache(
        &mut self,
        form: &mut Form,
        tools: &mut crate::tools::Tools,
        location: crate::settings::CacheLocation,
    ) {
        self.downloads = match location.store() {
            Ok(store) => Some(Downloads::unmeasured(store)),
            Err(error) => {
                log::warn!("cache unavailable: {error}");
                None
            }
        };
        form.set_cache_location(location);
        form.refresh_download(self.store());
        *tools = crate::tools::Tools::default();
    }

    fn show_settings(
        &mut self,
        ui: &mut egui::Ui,
        frame: &eframe::Frame,
        mut form: Form,
        mut tools: crate::tools::Tools,
    ) -> Screen {
        form.poll_preview(ui.ctx(), false);
        if let Some(usage) = form.take_preview_usage()
            && let Some(downloads) = &mut self.downloads
        {
            downloads.record_usage(usage);
        }
        tools.poll();
        if !form.preview_busy()
            && tools.installed().is_none()
            && !tools.busy()
            && tools.problem().is_none()
            && let Some(store) = self.store()
        {
            tools.start(
                store.directory().to_path_buf(),
                crate::tools::Operation::Scan,
                &crate::wake(ui.ctx()),
            );
        }
        let action = view::settings(ui, self.locale, &form, &tools, self.downloads.as_ref());
        match action {
            Some(view::SettingsAction::Back) if !tools.busy() => {
                if let Some(usage) = tools.usage()
                    && let Some(downloads) = &mut self.downloads
                {
                    downloads.record_usage(usage);
                }
                self.window_sizing = WindowSizing::Opening;
                return Screen::Editing(form);
            }
            Some(view::SettingsAction::Parallel(tracks)) => {
                form.set_parallel_tracks(tracks);
            }
            Some(view::SettingsAction::Tool(tool, action)) if !self.clearing() => {
                if let Some(store) = self.store() {
                    tools.start(
                        store.directory().to_path_buf(),
                        crate::tools::Operation::Manage(tool, action),
                        &crate::wake(ui.ctx()),
                    );
                }
            }
            Some(view::SettingsAction::OpenCache) if !tools.busy() => {
                if let Some(store) = self.store() {
                    tools.start(
                        store.directory().to_path_buf(),
                        crate::tools::Operation::OpenFolder,
                        &crate::wake(ui.ctx()),
                    );
                }
            }
            Some(view::SettingsAction::PickCache) if !tools.busy() && !form.preview_busy() => {
                if let Some(path) = dialogs::pick_folder(frame) {
                    match stem_split_core::domain::OutputDir::new(path) {
                        Ok(directory) => self.change_cache(
                            &mut form,
                            &mut tools,
                            crate::settings::CacheLocation::Custom(directory),
                        ),
                        Err(error) => log::warn!("invalid cache folder: {error}"),
                    }
                }
            }
            Some(view::SettingsAction::DefaultCache) if !tools.busy() && !form.preview_busy() => {
                self.change_cache(
                    &mut form,
                    &mut tools,
                    crate::settings::CacheLocation::UserCache,
                );
            }
            Some(view::SettingsAction::Cancel) => tools.cancel(),
            Some(
                view::SettingsAction::Back
                | view::SettingsAction::Tool(_, _)
                | view::SettingsAction::OpenCache
                | view::SettingsAction::PickCache
                | view::SettingsAction::DefaultCache,
            )
            | None => {}
        }
        Screen::Settings { form, tools }
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
            EditingAction::Settings => {
                if self.clearing() {
                    return Screen::Editing(form);
                }
                form.cancel_preview();
                let tools = crate::tools::Tools::default();
                return Screen::Settings { form, tools };
            }
            EditingAction::PlaylistPanel(panel) => {
                form.set_playlist_panel(panel);
                self.window_sizing = WindowSizing::PlaylistPanel;
            }
            EditingAction::PlaylistSelection(selection) => form.select_playlist(selection),
            EditingAction::PlaylistItem(index) => form.toggle_playlist_item(index),
            EditingAction::SaveFormat(format) => form.set_save_format(format),
            EditingAction::OutputFormat(format) => form.set_output_format(format),
            EditingAction::Download => {
                if self.clearing() {
                    return Screen::Editing(form);
                }
                return prepare_url(form, UrlAction::Download, context);
            }
            EditingAction::Source(source) => {
                let panel_was_visible = form.source() == Source::Url
                    && form.playlist().is_some_and(|(_, choices)| {
                        choices.panel() == crate::playlist::Panel::Open
                    });
                form.set_source(source);
                if panel_was_visible && source == Source::Files {
                    self.window_sizing = WindowSizing::PlaylistPanel;
                }
                if form.separation_panel() == crate::settings::SeparationPanel::Collapsed {
                    self.window_sizing = WindowSizing::Opening;
                }
            }
            EditingAction::SeparationPanel(panel) => {
                form.set_separation_panel(panel);
                self.window_sizing = WindowSizing::Opening;
            }
            EditingAction::Url(url) => {
                let panel_was_open = form
                    .playlist()
                    .is_some_and(|(_, choices)| choices.panel() == crate::playlist::Panel::Open);
                form.set_url(url);
                if panel_was_open {
                    self.window_sizing = WindowSizing::PlaylistPanel;
                }
            }
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
            EditingAction::ToggleStem(stem) => form.toggle_stem(stem),
            EditingAction::AllStems => form.select_all_stems(),
            EditingAction::NoStems => form.select_no_stems(),
            EditingAction::KeepStems(keep) => form.set_keep_stems(keep),
            EditingAction::Cleanup(enabled) => form.set_cleanup(enabled, self.store()),
            EditingAction::CleanupStems(stems) => form.set_cleanup_stems(stems),
            EditingAction::CleanupMix(mix) => form.set_cleanup_mix(mix),
            EditingAction::Enhancer(enhancer) => form.set_enhancer(enhancer, self.store()),
            EditingAction::Language(locale) => self.locale = locale,
            EditingAction::OpenDownloads => {
                if let Some(downloads) = &mut self.downloads {
                    downloads.open_folder();
                }
            }
            EditingAction::ClearDownloads => {
                if form.preview_busy() {
                    return Screen::Editing(form);
                }
                form.cancel_preview();
                if let Some(downloads) = &mut self.downloads {
                    downloads.clear(&crate::wake(context));
                }
            }
            EditingAction::Start(mode) => {
                if self.clearing() {
                    return Screen::Editing(form);
                }
                return prepare(form, mode, OverwritePolicy::Refuse, context);
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
            if let Event::SourceReady(input, action) = event {
                let mut form = run.form;
                let results = match run.tracker.finish(Channel::Ended, now) {
                    End::Results(results) => results,
                    End::Cancelled => Results {
                        files: Vec::new(),
                        took: Duration::ZERO,
                    },
                    End::NotFetched(failure) => return Screen::Failed { form, failure },
                };
                if let Some(downloads) = &mut self.downloads {
                    downloads.refresh();
                }
                let mode = match action {
                    UrlAction::Download => return source_done(form, input, results),
                    UrlAction::Separate(mode) => mode,
                };
                if input.is_empty() {
                    return source_done(form, input, results);
                }
                let failures = results
                    .files
                    .into_iter()
                    .filter(|file| matches!(file.outcome, Outcome::Failed(_) | Outcome::Cancelled))
                    .collect();
                form.set_downloads(input, failures);
                if run.stopping == Stopping::Requested {
                    return Screen::Editing(form);
                }
                return prepare_batch(form, mode, OverwritePolicy::Refuse, ui.ctx());
            }
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
            End::Results(mut results) => {
                results.files.extend(form.take_download_failures());
                Screen::Done(Finished {
                    form,
                    results,
                    problem: None,
                })
            }
        }
    }
}

fn source_done(
    mut form: Form,
    input: Vec<stem_split_core::media_fetching::DownloadedAudio>,
    results: Results,
) -> Screen {
    form.set_downloads(input, Vec::new());
    if results.files.is_empty() {
        return Screen::Editing(form);
    }
    Screen::Done(Finished {
        form,
        results,
        problem: None,
    })
}

/// Starts the batch the form describes for `mode`, unless its files clash
/// or results of an earlier run are in the way and `overwrite` does not
/// allow replacing them, in which case the user is asked first.
fn prepare(
    mut form: Form,
    mode: Mode,
    overwrite: OverwritePolicy,
    context: &egui::Context,
) -> Screen {
    match form.source() {
        Source::Files => prepare_batch(form, mode, overwrite, context),
        Source::Url => {
            if form.review_playlist() {
                return Screen::Editing(form);
            }
            if form.inputs().is_some_and(|inputs| {
                inputs.iter().all(|input| input.path().is_file())
                    && form.download_sources().all(Path::is_file)
            }) {
                return prepare_batch(form, mode, overwrite, context);
            }
            prepare_url(form, UrlAction::Separate(mode), context)
        }
    }
}

fn prepare_url(mut form: Form, action: UrlAction, context: &egui::Context) -> Screen {
    let Some((url, folder)) = form.url_request() else {
        return Screen::Editing(form);
    };
    if action == UrlAction::Separate(Mode::SplitAndMix) && !form.can_mix() {
        return Screen::Editing(form);
    }
    if form.review_playlist() {
        return Screen::Editing(form);
    }
    form.cancel_preview();
    let tracker = Tracker::for_url(form.model(), &url, folder.path(), Instant::now());
    match Worker::start_url(
        url,
        folder,
        form.save_format(),
        action,
        crate::wake(context),
        form.cache_location().clone(),
    ) {
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

fn prepare_batch(
    mut form: Form,
    mode: Mode,
    overwrite: OverwritePolicy,
    context: &egui::Context,
) -> Screen {
    match form.batch(mode, overwrite) {
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
                Screen::Confirming {
                    form,
                    mode,
                    existing,
                }
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
        Some(DoneAction::Back) => {
            finished.form.remember_download_failures(
                finished
                    .results
                    .files
                    .into_iter()
                    .filter(|file| matches!(file.outcome, Outcome::Failed(Failure::Media(_))))
                    .collect(),
            );
            Screen::Editing(finished.form)
        }
        None => Screen::Done(finished),
    }
}

/// Starts the batch and shows its progress, or the reason it could not start.
fn launch(form: Form, batch: Batch, context: &egui::Context) -> Screen {
    let mut tracker = Tracker::new(&batch, Instant::now());
    if form.source() == Source::Url {
        tracker.name_sources(form.download_sources());
    }
    match Worker::start(batch, crate::wake(context), form.cache_location().clone()) {
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
        let error_color = if ui.visuals().dark_mode {
            egui::Color32::from_rgb(195, 116, 116)
        } else {
            egui::Color32::from_rgb(161, 77, 77)
        };
        ui.visuals_mut().error_fg_color = error_color;
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
        self.screen.form_mut().stop_preview(EXIT_PATIENCE);
        match mem::take(&mut self.screen) {
            // Give the jobs the chance to remove their unfinished files.
            Screen::Running(run) => run.worker.stop(EXIT_PATIENCE),
            Screen::Settings { mut tools, .. } => tools.stop(EXIT_PATIENCE),
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

    #[test]
    fn switching_cache_updates_the_form_and_preserves_previous_files() -> Result<(), Box<dyn Error>>
    {
        use crate::settings::CacheLocation;
        use stem_split_core::domain::OutputDir;
        let directory = tempfile::tempdir()?;
        let previous = directory.path().join("previous");
        let next = directory.path().join("next");
        fs::create_dir(&previous)?;
        fs::create_dir(&next)?;
        let previous_file = previous.join("model.onnx");
        fs::write(&previous_file, b"model")?;
        let mut settings = Settings::initial();
        settings.cache_location = CacheLocation::Custom(OutputDir::new(&previous)?);
        let mut application = StemSplitApp::new(&settings, &[]);
        assert_eq!(
            application.store().map(ModelStore::directory),
            Some(previous.as_path())
        );
        let mut form = Form::with(&settings);
        let mut tools = crate::tools::Tools::default();
        application.change_cache(
            &mut form,
            &mut tools,
            CacheLocation::Custom(OutputDir::new(&next)?),
        );
        assert_eq!(
            application.store().map(ModelStore::directory),
            Some(next.as_path())
        );
        assert_eq!(form.cache_location().store()?.directory(), next);
        assert_eq!(fs::read(previous_file)?, b"model");
        assert!(fs::read_dir(&next)?.next().is_none());
        assert!(!tools.busy());
        Ok(())
    }

    #[test]
    fn every_url_start_reviews_a_large_playlist_before_starting_a_worker()
    -> Result<(), Box<dyn Error>> {
        use crate::{playlist::Panel, preview::State};
        use stem_split_core::media_fetching::{MediaInfo, MediaItem, MediaUrl};
        let directory = tempfile::tempdir()?;
        let context = egui::Context::default();
        for action in [
            UrlAction::Download,
            UrlAction::Separate(Mode::Split),
            UrlAction::Separate(Mode::SplitAndMix),
        ] {
            let url = MediaUrl::new("https://example.org/playlist")?;
            let mut form = Form::default();
            form.set_source(Source::Url);
            form.set_url(url.as_str().to_owned());
            form.set_output_folder(directory.path());
            form.select_all_stems();
            form.show_preview(State::Ready(MediaInfo::Playlist {
                title: "Playlist".to_owned(),
                first: MediaItem::new("First".to_owned(), url.clone()),
                rest: (1..6)
                    .map(|number| MediaItem::new(format!("Track {number}"), url.clone()))
                    .collect(),
            }));
            let next = match action {
                UrlAction::Download => prepare_url(form, action, &context),
                UrlAction::Separate(mode) => prepare(form, mode, OverwritePolicy::Refuse, &context),
            };
            let Screen::Editing(form) = next else {
                return Err("start bypassed playlist review".into());
            };
            assert_eq!(
                form.playlist().map(|(_, choices)| choices.panel()),
                Some(Panel::Open)
            );
        }
        Ok(())
    }

    #[test]
    fn window_minimum_is_limited_by_a_small_monitor() -> Result<(), io::Error> {
        let context = egui::Context::default();
        let mut application = StemSplitApp {
            locale: Locale::En,
            downloads: None,
            screen: Screen::default(),
            window_sizing: WindowSizing::Opening,
        };
        let mut input = egui::RawInput::default();
        let viewport = input.viewports.entry(egui::ViewportId::ROOT).or_default();
        viewport.monitor_size = Some(egui::vec2(400.0, 300.0));
        viewport.inner_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(640.0, 540.0),
        ));
        viewport.outer_rect = Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(640.0, 572.0),
        ));
        let output = context.run_ui(input, |ui| {
            application.fit_window(ui.ctx(), egui::vec2(640.0, 600.0));
        });
        let viewport = output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .ok_or_else(|| io::Error::other("root viewport output is absent"))?;
        let sizes: Vec<_> = viewport
            .commands
            .iter()
            .filter_map(|command| match command {
                egui::ViewportCommand::MinInnerSize(size)
                | egui::ViewportCommand::InnerSize(size) => Some(size),
                _ => None,
            })
            .collect();
        assert_eq!(sizes.len(), 2);
        for size in sizes {
            assert!(size.x > 0.0 && size.x < 400.0 && size.y > 0.0 && size.y < 300.0);
        }
        output.drop_without_applying_deltas();
        Ok(())
    }

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
            mode: Mode::SplitAndMix,
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
        assert_eq!(inputs_of(&app.screen), Vec::<&std::path::Path>::new());
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
