//! Choices remembered between launches.

use stem_split_core::{
    domain::{EnhancerId, ModelId, OutputChoice, OutputDir, Stem, StemSet},
    separate::DevicePreference,
};

use crate::view::text::Locale;

const LOCALE: &str = "locale";
const MODEL: &str = "model";
const DEVICE: &str = "device";
const OUTPUT_FOLDER: &str = "output_folder";
const MIX_STEMS: &str = "mix_stems";
const KEEP_STEMS: &str = "keep_stems";
const CLEANUP: &str = "cleanup";
const CLEANUP_STEMS: &str = "cleanup_stems";
const CLEANUP_MIX: &str = "cleanup_mix";
const ENHANCER: &str = "enhancer";

/// How a mix is made: from which stems, and whether the stems are saved
/// as well.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MixChoice {
    /// The selection over every stem of every model, so that a stem only
    /// one model has stays selected while another model is chosen.
    pub(crate) stems: StemSet,
    pub(crate) keep_stems: bool,
}

impl MixChoice {
    /// Every stem, without the stem files: unticking what is not wanted is
    /// the whole point of a mix.
    fn initial() -> Self {
        Self {
            stems: Stem::ALL
                .iter()
                .fold(StemSet::none(), |stems, &stem| stems.toggled(stem)),
            keep_stems: false,
        }
    }
}

/// Whether and how the results are cleaned up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CleanupChoice {
    pub(crate) enabled: bool,
    /// Whether the stems are cleaned: in "Split" the only thing there is to
    /// clean, in "Split and mix" before the mix is summed from them.
    pub(crate) stems: bool,
    /// Whether the finished mix is cleaned, in "Split and mix".
    pub(crate) mix: bool,
    pub(crate) enhancer: EnhancerId,
}

impl CleanupChoice {
    /// Off: the cleanup takes time and changes the sound, so it is asked for.
    fn initial() -> Self {
        Self {
            enabled: false,
            stems: true,
            mix: false,
            enhancer: EnhancerId::default(),
        }
    }

    /// The network to run, if the cleanup is on.
    pub(crate) fn enhancer(self) -> Option<EnhancerId> {
        self.enabled.then_some(self.enhancer)
    }
}

/// What the application remembers: the language and how to split, but not
/// which file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Settings {
    pub(crate) locale: Locale,
    pub(crate) model: ModelId,
    pub(crate) device: DevicePreference,
    pub(crate) output: OutputChoice,
    pub(crate) mix: MixChoice,
    pub(crate) cleanup: CleanupChoice,
}

/// The name a device preference is saved under. Unlike the label shown in
/// the interface, it must never change, whatever the language.
const fn device_key(device: DevicePreference) -> &'static str {
    match device {
        DevicePreference::Auto => "auto",
        DevicePreference::Gpu => "gpu",
        DevicePreference::Cpu => "cpu",
    }
}

const fn switch_key(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

fn switch_from(key: &str) -> Option<bool> {
    match key {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    }
}

/// Stems saved as their file tags, comma-separated: `drums,bass`.
fn stems_key(stems: StemSet) -> String {
    stems
        .iter_all()
        .map(Stem::file_tag)
        .collect::<Vec<_>>()
        .join(",")
}

/// The stems of a saved list; `None` if any of its names is unknown, so
/// that a damaged list falls back as a whole. An empty list is a valid one.
fn stems_from(key: &str) -> Option<StemSet> {
    key.split(',')
        .filter(|name| !name.is_empty())
        .try_fold(StemSet::none(), |stems, name| {
            let stem = Stem::ALL
                .iter()
                .copied()
                .find(|stem| stem.file_tag() == name)?;
            Some(stems.toggled(stem))
        })
}

impl Settings {
    /// What a first launch starts with: the language of the system and the
    /// defaults of everything else.
    pub(crate) fn initial() -> Self {
        Self {
            locale: Locale::of_system(),
            model: ModelId::default(),
            device: DevicePreference::default(),
            output: OutputChoice::default(),
            mix: MixChoice::initial(),
            cleanup: CleanupChoice::initial(),
        }
    }

    /// Reads what an earlier launch saved.
    ///
    /// Each value is checked like any other outside input. One that is
    /// missing, unknown to this version or no longer valid (a folder that
    /// has been deleted) falls back to its initial value on its own, without
    /// discarding the others.
    pub(crate) fn load(storage: &dyn eframe::Storage) -> Self {
        let initial = Self::initial();
        let read = |key| storage.get_string(key);
        Self {
            locale: read(LOCALE)
                .and_then(|code| Locale::from_code(&code))
                .unwrap_or(initial.locale),
            model: read(MODEL)
                .and_then(|name| {
                    ModelId::ALL
                        .iter()
                        .copied()
                        .find(|model| model.name() == name)
                })
                .unwrap_or(initial.model),
            device: read(DEVICE)
                .and_then(|key| {
                    DevicePreference::ALL
                        .iter()
                        .copied()
                        .find(|&device| device_key(device) == key)
                })
                .unwrap_or(initial.device),
            output: read(OUTPUT_FOLDER)
                .and_then(|path| OutputDir::new(path).ok())
                .map_or(initial.output, OutputChoice::Custom),
            mix: MixChoice {
                stems: read(MIX_STEMS)
                    .and_then(|key| stems_from(&key))
                    .unwrap_or(initial.mix.stems),
                keep_stems: read(KEEP_STEMS)
                    .and_then(|key| switch_from(&key))
                    .unwrap_or(initial.mix.keep_stems),
            },
            cleanup: CleanupChoice {
                enabled: read(CLEANUP)
                    .and_then(|key| switch_from(&key))
                    .unwrap_or(initial.cleanup.enabled),
                stems: read(CLEANUP_STEMS)
                    .and_then(|key| switch_from(&key))
                    .unwrap_or(initial.cleanup.stems),
                mix: read(CLEANUP_MIX)
                    .and_then(|key| switch_from(&key))
                    .unwrap_or(initial.cleanup.mix),
                enhancer: read(ENHANCER)
                    .and_then(|name| {
                        EnhancerId::ALL
                            .iter()
                            .copied()
                            .find(|enhancer| enhancer.name() == name)
                    })
                    .unwrap_or(initial.cleanup.enhancer),
            },
        }
    }

    pub(crate) fn save(&self, storage: &mut dyn eframe::Storage) {
        storage.set_string(LOCALE, self.locale.code().to_owned());
        storage.set_string(MODEL, self.model.name().to_owned());
        storage.set_string(DEVICE, device_key(self.device).to_owned());
        storage.set_string(MIX_STEMS, stems_key(self.mix.stems));
        storage.set_string(KEEP_STEMS, switch_key(self.mix.keep_stems).to_owned());
        storage.set_string(CLEANUP, switch_key(self.cleanup.enabled).to_owned());
        storage.set_string(CLEANUP_STEMS, switch_key(self.cleanup.stems).to_owned());
        storage.set_string(CLEANUP_MIX, switch_key(self.cleanup.mix).to_owned());
        storage.set_string(ENHANCER, self.cleanup.enhancer.name().to_owned());
        let folder = match &self.output {
            OutputChoice::BesideInput => None,
            // A path that is not valid Unicode cannot be stored as text; the
            // next launch then starts with the default.
            OutputChoice::Custom(directory) => directory.path().to_str(),
        };
        match folder {
            Some(path) => storage.set_string(OUTPUT_FOLDER, path.to_owned()),
            None => storage.remove_string(OUTPUT_FOLDER),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, error::Error};

    use eframe::Storage as _;

    use super::*;

    /// Settings storage that lives in memory.
    #[derive(Debug, Default)]
    struct MemoryStorage(HashMap<String, String>);

    impl eframe::Storage for MemoryStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }

        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }

        fn flush(&mut self) {}
    }

    fn storage_with(entries: &[(&str, &str)]) -> MemoryStorage {
        MemoryStorage(
            entries
                .iter()
                .map(|&(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        )
    }

    #[test]
    fn empty_storage_gives_the_initial_settings() {
        assert_eq!(
            Settings::load(&MemoryStorage::default()),
            Settings::initial()
        );
        assert_eq!(Settings::initial().model, ModelId::Htdemucs);
        assert_eq!(Settings::initial().device, DevicePreference::Auto);
        assert_eq!(Settings::initial().output, OutputChoice::BesideInput);
    }

    #[test]
    fn saved_settings_are_read_back() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let settings = Settings {
            locale: Locale::Ru,
            model: ModelId::Htdemucs6s,
            device: DevicePreference::Cpu,
            output: OutputChoice::Custom(OutputDir::new(directory.path())?),
            mix: MixChoice {
                stems: StemSet::none().toggled(Stem::Piano).toggled(Stem::Bass),
                keep_stems: true,
            },
            cleanup: CleanupChoice {
                enabled: true,
                stems: false,
                mix: true,
                enhancer: EnhancerId::VocalBleed,
            },
        };
        let mut storage = MemoryStorage::default();

        settings.save(&mut storage);

        assert_eq!(Settings::load(&storage), settings);
        Ok(())
    }

    #[test]
    fn every_model_device_and_language_survives_a_round_trip() {
        for &model in ModelId::ALL {
            for &device in DevicePreference::ALL {
                for &locale in Locale::ALL {
                    let settings = Settings {
                        locale,
                        model,
                        device,
                        ..Settings::initial()
                    };
                    let mut storage = MemoryStorage::default();
                    settings.save(&mut storage);
                    assert_eq!(Settings::load(&storage), settings);
                }
            }
        }
    }

    #[test]
    fn choosing_the_folder_of_the_file_forgets_the_other_folder() -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let mut settings = Settings {
            output: OutputChoice::Custom(OutputDir::new(directory.path())?),
            ..Settings::initial()
        };
        let mut storage = MemoryStorage::default();
        settings.save(&mut storage);

        settings.output = OutputChoice::BesideInput;
        settings.save(&mut storage);

        assert_eq!(storage.get_string(OUTPUT_FOLDER), None);
        assert_eq!(Settings::load(&storage).output, OutputChoice::BesideInput);
        Ok(())
    }

    #[test]
    fn folder_that_no_longer_exists_falls_back_without_losing_the_rest()
    -> Result<(), Box<dyn Error>> {
        let directory = tempfile::tempdir()?;
        let gone = directory.path().join("deleted since");
        let storage = storage_with(&[
            (LOCALE, "ru"),
            (MODEL, "htdemucs_ft"),
            (DEVICE, "gpu"),
            (OUTPUT_FOLDER, gone.to_str().ok_or("path is not Unicode")?),
        ]);

        let settings = Settings::load(&storage);

        assert_eq!(settings.output, OutputChoice::BesideInput);
        assert_eq!(settings.locale, Locale::Ru);
        assert_eq!(settings.model, ModelId::HtdemucsFt);
        assert_eq!(settings.device, DevicePreference::Gpu);
        Ok(())
    }

    #[test]
    fn unknown_values_fall_back_one_by_one() {
        let storage = storage_with(&[
            (LOCALE, "tlh"),
            (MODEL, "a model of the future"),
            (DEVICE, "cpu"),
        ]);

        let settings = Settings::load(&storage);

        assert_eq!(settings.locale, Settings::initial().locale);
        assert_eq!(settings.model, ModelId::Htdemucs);
        assert_eq!(settings.device, DevicePreference::Cpu);
    }

    #[test]
    fn every_stem_selection_and_cleanup_survives_a_round_trip() {
        let none = StemSet::none();
        for stems in [none, none.toggled(Stem::Vocals), MixChoice::initial().stems] {
            for (cleaned_stems, cleaned_mix) in [(true, false), (false, true), (true, true)] {
                for &enhancer in EnhancerId::ALL {
                    let settings = Settings {
                        mix: MixChoice {
                            stems,
                            keep_stems: true,
                        },
                        cleanup: CleanupChoice {
                            enabled: true,
                            stems: cleaned_stems,
                            mix: cleaned_mix,
                            enhancer,
                        },
                        ..Settings::initial()
                    };
                    let mut storage = MemoryStorage::default();
                    settings.save(&mut storage);
                    assert_eq!(Settings::load(&storage), settings);
                }
            }
        }
    }

    #[test]
    fn damaged_mix_and_cleanup_values_fall_back() {
        let storage = storage_with(&[
            (MIX_STEMS, "drums,kazoo"),
            (KEEP_STEMS, "maybe"),
            (CLEANUP, "on"),
            (CLEANUP_STEMS, "everything"),
            (CLEANUP_MIX, "on"),
            (ENHANCER, "a network of the future"),
        ]);

        let settings = Settings::load(&storage);

        let initial = Settings::initial();
        assert_eq!(settings.mix, initial.mix);
        assert!(settings.cleanup.enabled);
        assert_eq!(settings.cleanup.stems, initial.cleanup.stems);
        assert!(settings.cleanup.mix);
        assert_eq!(settings.cleanup.enhancer, initial.cleanup.enhancer);
    }

    #[test]
    fn device_keys_are_distinct() {
        let keys: std::collections::HashSet<&str> = DevicePreference::ALL
            .iter()
            .map(|&device| device_key(device))
            .collect();
        assert_eq!(keys.len(), DevicePreference::ALL.len());
    }
}
