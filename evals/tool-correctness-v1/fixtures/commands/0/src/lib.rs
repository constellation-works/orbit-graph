use clap::Subcommand;
#[derive(Subcommand)]
pub enum Commands { Run(RunArgs) }
pub struct RunArgs;
pub fn dispatch(command: Commands) { match command { Commands::Run(args) => run(args) } }
pub fn run(_args: RunArgs) { middle(); }
pub fn middle() { leaf(); }
pub fn leaf() {}
