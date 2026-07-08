//! Build identity helpers.

pub const BASE_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn channel() -> &'static str {
    non_empty(option_env!("HERDR_BUILD_CHANNEL")).unwrap_or("stable")
}

pub fn build_id() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_ID"))
}

pub fn build_commit() -> Option<&'static str> {
    non_empty(option_env!("HERDR_BUILD_COMMIT"))
}

pub fn version() -> String {
    version_for(channel(), build_id(), build_commit())
}

fn version_for(channel: &str, build_id: Option<&str>, build_commit: Option<&str>) -> String {
    match channel {
        // A stable-channel build stamped with a build number and/or commit is
        // a fork build; the suffix distinguishes it from the upstream release
        // of the same base version. The build number orders fork builds within
        // a base version; the commit is traceability-only build metadata.
        "stable" => match (build_id, build_commit) {
            (Some(id), Some(commit)) => format!("{BASE_VERSION}-{id}+{commit}"),
            (Some(id), None) => format!("{BASE_VERSION}-{id}"),
            (None, Some(commit)) => format!("{BASE_VERSION}-{commit}"),
            (None, None) => BASE_VERSION.to_string(),
        },
        channel => match build_id {
            Some(build_id) => format!("{BASE_VERSION}-{channel}.{build_id}"),
            None => format!("{BASE_VERSION}-{channel}"),
        },
    }
}

pub fn is_preview() -> bool {
    channel() == "preview"
}

fn non_empty(value: Option<&'static str>) -> Option<&'static str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{version_for, BASE_VERSION};

    #[test]
    fn stable_version_defaults_to_cargo_version() {
        assert!(!super::version().is_empty());
    }

    #[test]
    fn stable_version_without_commit_is_base_version() {
        assert_eq!(version_for("stable", None, None), BASE_VERSION);
    }

    #[test]
    fn stable_version_with_commit_appends_short_sha() {
        assert_eq!(
            version_for("stable", None, Some("f2634a6")),
            format!("{BASE_VERSION}-f2634a6")
        );
    }

    #[test]
    fn stable_version_with_build_number_and_commit_orders_then_traces() {
        assert_eq!(
            version_for("stable", Some("15"), Some("f2634a6")),
            format!("{BASE_VERSION}-15+f2634a6")
        );
    }

    #[test]
    fn stable_version_with_build_number_only_appends_number() {
        assert_eq!(
            version_for("stable", Some("15"), None),
            format!("{BASE_VERSION}-15")
        );
    }

    #[test]
    fn preview_version_ignores_build_commit() {
        assert_eq!(
            version_for("preview", Some("2026-07-07-abcdef123456"), Some("abcdef1")),
            format!("{BASE_VERSION}-preview.2026-07-07-abcdef123456")
        );
    }

    #[test]
    fn non_stable_channel_without_build_id_appends_channel() {
        assert_eq!(
            version_for("preview", None, None),
            format!("{BASE_VERSION}-preview")
        );
    }
}
