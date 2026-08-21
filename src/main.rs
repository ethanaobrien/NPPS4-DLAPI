// Copyright (c) 2023 Dark Energy Processor
//
// This software is provided 'as-is', without any express or implied
// warranty. In no event will the authors be held liable for any damages
// arising from the use of this software.
//
// Permission is granted to anyone to use this software for any purpose,
// including commercial applications, and to alter it and redistribute it
// freely, subject to the following restrictions:
//
// 1. The origin of this software must not be misrepresented; you must not
//    claim that you wrote the original software. If you use this software
//    in a product, an acknowledgment in the product documentation would be
//    appreciated but is not required.
// 2. Altered source versions must be plainly marked as such, and must not be
//    misrepresented as being the original software.
// 3. This notice may not be removed or altered from any source distribution.

mod clone_cmd;
mod config;
mod file_handler;
mod honkypy;
mod models;
mod serve;
mod upgrade;
mod util;

use clap::{Parser, Subcommand};

/// NPPS4 Download API server and archive management tools.
///
/// Run without a subcommand to start the download API server.
#[derive(Parser)]
#[command(name = "n4dlapi", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the download API server (default when no subcommand given).
    Serve(serve::ServeArgs),
    /// Upgrade an archive-root to the latest generation (currently 1.2).
    ///
    /// Runs the required stages in order. 1.0 -> 1.1 hashes all archives and
    /// writes infov2.json metadata files, extracts microdl files, and decrypts
    /// game databases (update_v1.1.py). 1.1 -> 1.2 renames archives to unique
    /// "{n}_{sha256}.zip" names to prevent in-game download corruption
    /// (update_v1.2.py).
    Upgrade(upgrade::UpgradeArgs),
    /// Clone a remote NPPS4-DLAPI server's archive to a local directory.
    ///
    /// Requires access to a running NPPS4-DLAPI v1.1 server.
    Clone(clone_cmd::CloneArgs),
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        None => {
            tokio::runtime::Runtime::new()?
                .block_on(serve::run(serve::ServeArgs::default()))
        }
        Some(Commands::Serve(args)) => {
            tokio::runtime::Runtime::new()?.block_on(serve::run(args))
        }
        Some(Commands::Upgrade(args)) => upgrade::run(args),
        Some(Commands::Clone(args)) => clone_cmd::run(args),
    }
}
