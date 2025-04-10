use clap::Parser;

/// Quickly create http tunnels for development
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// Make all tunnels public by default instead of private
    #[arg(long, group = "access")]
    public: bool,

    #[arg(long, group = "access")]
    protected: bool,
}

impl Args {
    pub fn make_public(&self) -> bool {
        self.public
    }

    pub fn make_protected(&self) -> bool {
        self.protected
    }
}
