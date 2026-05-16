mod fixtures;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use mig::{FileDiffKind, GitState, WatchBackend, Workspace, WorkspaceDiff, diff_engine, watch};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(version, about = "Live diff memory for agent-inhabited worktrees")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Open a hardcoded patch-review UI fixture.
    Tryout {
        /// Fixture scene to render.
        #[arg(value_enum, default_value_t = tui::TryoutScene::InlineChange)]
        scene: tui::TryoutScene,

        /// Palette theme. Auto currently falls back to dark until terminal probing lands.
        #[arg(long, value_enum, default_value_t = tui::TuiTheme::Auto)]
        theme: tui::TuiTheme,
    },
    /// Print a one-shot L2 workspace and L1 git working-tree summary.
    Status {
        /// Workspace directory to inspect.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Print all files currently visible to Mig's L2 filesystem scanner.
    Files {
        /// Workspace directory to inspect.
        #[arg(default_value = ".")]
        path: PathBuf,
    },
    /// Watch the workspace and print a diff for each filesystem turn.
    Watch {
        /// Workspace directory to inspect.
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Use Mig's polling watcher instead of platform-native filesystem events.
        #[arg(long)]
        poll: bool,

        /// Open the Ratatui diff viewer for the first text file changed in each filesystem turn.
        #[arg(long)]
        tui: bool,

        /// Milliseconds to wait for adjacent fs events before closing a turn.
        #[arg(long, default_value_t = 250)]
        debounce_ms: u64,

        /// Milliseconds between filesystem polls when using --poll.
        #[arg(long, default_value_t = 500)]
        poll_interval_ms: u64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command.unwrap_or_else(default_command) {
        Command::Tryout { scene, theme } => tui::run_tryout(scene, theme),
        Command::Status { path } => status(path),
        Command::Files { path } => files(path),
        Command::Watch {
            path,
            poll,
            tui,
            debounce_ms,
            poll_interval_ms,
        } => watch_cli(path, poll, tui, debounce_ms, poll_interval_ms),
    }
}

fn default_command() -> Command {
    Command::Watch {
        path: PathBuf::from("."),
        poll: false,
        tui: false,
        debounce_ms: 250,
        poll_interval_ms: 500,
    }
}

fn status(path: PathBuf) -> Result<()> {
    let workspace = Workspace::open(path)?;
    let snapshot = workspace.snapshot()?;
    println!("mig workspace: {}", workspace.root().display());
    println!("L2 files: {}", snapshot.file_count());
    print_git_state(workspace.git_state()?.as_ref());
    Ok(())
}

fn files(path: PathBuf) -> Result<()> {
    let workspace = Workspace::open(path)?;
    let snapshot = workspace.snapshot()?;
    println!("mig workspace: {}", workspace.root().display());
    println!("L2 files: {}", snapshot.file_count());
    for path in snapshot.paths() {
        println!("{}", path.display());
    }
    Ok(())
}

fn watch_cli(
    path: PathBuf,
    poll: bool,
    open_tui: bool,
    debounce_ms: u64,
    poll_interval_ms: u64,
) -> Result<()> {
    let workspace = Workspace::open(path)?;
    let baseline = workspace.snapshot()?;
    let backend = if poll {
        WatchBackend::Poll {
            interval: Duration::from_millis(poll_interval_ms),
        }
    } else {
        WatchBackend::Native
    };

    println!("mig watch: {}", workspace.root().display());
    println!("watch backend: {}", if poll { "poll" } else { "native" });
    println!("turn viewer: {}", if open_tui { "tui" } else { "text" });
    println!("L2 baseline: {} files", baseline.file_count());
    print_git_state(workspace.git_state()?.as_ref());
    println!("watching for filesystem turns...");

    watch(
        workspace,
        backend,
        Duration::from_millis(debounce_ms),
        |turn| -> Result<()> {
            let stats = turn.diff.stats();
            println!(
                "\nturn #{} at {} ({} fs events, {} files: +{} ~{} -{})",
                turn.index,
                turn.occurred_at,
                turn.event_count,
                turn.diff.len(),
                stats.added,
                stats.modified,
                stats.deleted
            );
            if open_tui {
                open_turn_tui(&turn.diff)?;
            } else {
                print_workspace_diff(&turn.diff);
            }
            print_git_state(turn.git.as_ref());
            Ok(())
        },
    )
}

fn open_turn_tui(diff: &WorkspaceDiff) -> Result<()> {
    let Some(state) = diff
        .files()
        .iter()
        .find_map(|file| diff_engine::file_diff_state(file, "L2 previous", "L2 current"))
    else {
        println!("no text file diff available for tui");
        return Ok(());
    };

    tui::run_state(&state, tui::TuiTheme::Auto)
}

fn print_workspace_diff(diff: &WorkspaceDiff) {
    for file in diff.files() {
        let kind = match file.kind() {
            FileDiffKind::Added => "added",
            FileDiffKind::Modified => "modified",
            FileDiffKind::Deleted => "deleted",
        };
        println!("{} {}", file.kind().marker(), file.path().display());
        println!("# {}", kind);
        print!("{}", file.render_unified());
    }
}

fn print_git_state(git: Option<&GitState>) {
    let Some(git) = git else {
        println!("L1 git: unavailable");
        return;
    };

    println!("L1 git root: {}", git.root.display());
    if git.is_clean() {
        println!("L1 git: clean");
        return;
    }

    println!("L1 git status:");
    for line in &git.status {
        println!("  {line}");
    }
    if !git.diff_stat.is_empty() {
        println!("L1 git diff stat:");
        for line in &git.diff_stat {
            println!("  {line}");
        }
    }
}
