//! What every part of Master Control shares: the shape of an observed event and the store a session keeps them in.

mod event;
pub mod link;
mod store;

pub use event::{Class, Event};
pub use store::{Filter, Store, StoreError};

/// The version of this build: the major.minor of the `VERSION` file, then the commits since it last changed.
pub const VERSION: &str = env!("MC_VERSION");
/// What the About screen and the command lines say this is.
pub const DESCRIPTION: &str = "Lets AI coding agents drive and watch your computers.";
pub const AUTHOR: &str = "dunkelherzmann";
pub const PROJECT: &str = "https://github.com/dunkelherzmann/master-control";

#[cfg(test)]
mod version_tests {
    #[test]
    fn the_version_is_the_files_major_and_minor_with_a_patch_number() {
        let base = include_str!("../../VERSION").trim();
        let patch = super::VERSION.strip_prefix(base).and_then(|rest| rest.strip_prefix('.')).unwrap_or_else(|| panic!("{} doesn't start with {base}.", super::VERSION));
        assert!(patch.parse::<u32>().is_ok(), "{}", super::VERSION);
        assert_eq!(base.split('.').count(), 2, "VERSION holds major.minor only");
    }
}
