use crate::closed_enum;

closed_enum! {
    /// Networks that clean separated audio of artifacts.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub enum EnhancerId {
        /// `Reverb_HQ_By_FoxJoy`, an MDX-Net network that predicts reverb
        /// and smeared tails, which are then subtracted.
        #[default]
        ReverbHq,
        /// `UVR-MDX-NET-Voc_FT`, an MDX-Net network that predicts vocals. It
        /// takes leaked instruments out of the vocals and leaked vocals out
        /// of everything else.
        VocalBleed,
    }
}

impl EnhancerId {
    /// A stable identifier: it names the network in saved settings and on
    /// the command line, and stays the same in every interface language.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ReverbHq => "reverb_hq",
            Self::VocalBleed => "vocal_bleed",
        }
    }
}

/// What the cleanup of a job that makes a mix is applied to: the stems,
/// the mix, or both. Never nothing; a job that cleans nothing has no cleanup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CleanupTargets {
    /// Every stem the job uses, before it is written or mixed.
    Stems,
    /// Only the finished mix: one pass instead of one per stem.
    Mix,
    /// The stems, and then the mix summed from them.
    Both,
}

impl CleanupTargets {
    /// The targets two ticks describe; `None` when neither is ticked.
    #[must_use]
    pub const fn from_ticks(stems: bool, mix: bool) -> Option<Self> {
        match (stems, mix) {
            (true, false) => Some(Self::Stems),
            (false, true) => Some(Self::Mix),
            (true, true) => Some(Self::Both),
            (false, false) => None,
        }
    }

    #[must_use]
    pub const fn stems(self) -> bool {
        match self {
            Self::Stems | Self::Both => true,
            Self::Mix => false,
        }
    }

    #[must_use]
    pub const fn mix(self) -> bool {
        match self {
            Self::Mix | Self::Both => true,
            Self::Stems => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn enhancer_names_are_distinct() {
        let names: HashSet<&str> = EnhancerId::ALL.iter().map(|id| id.name()).collect();
        assert_eq!(names.len(), EnhancerId::ALL.len());
    }

    #[test]
    fn targets_follow_the_ticks() {
        for stems in [false, true] {
            for mix in [false, true] {
                let targets = CleanupTargets::from_ticks(stems, mix);
                assert_eq!(
                    targets.map(CleanupTargets::stems),
                    (stems || mix).then_some(stems)
                );
                assert_eq!(
                    targets.map(CleanupTargets::mix),
                    (stems || mix).then_some(mix)
                );
            }
        }
    }
}
