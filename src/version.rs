pub const VERSION: &str = get_version();

const fn get_version() -> &'static str {
    if let Some(version) = std::option_env!("RELEASE_VERSION")
        && !version.is_empty()
    {
        version
    } else {
        git_version::git_version!(fallback = "unknown")
    }
}
