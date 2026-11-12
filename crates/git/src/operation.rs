use crate::repository::RemoteCommandOutput;
use crate::{REBASE_APPLY_DIR, REBASE_MERGE_DIR, SEQUENCER_DIR};
use anyhow::{Result, bail};
use gpui::SharedString;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GitOperationKind {
    Merge,
    CherryPick,
    Revert,
    Rebase,
}

impl GitOperationKind {
    pub fn subcommand(self) -> &'static str {
        match self {
            Self::Merge => "merge",
            Self::CherryPick => "cherry-pick",
            Self::Revert => "revert",
            Self::Rebase => "rebase",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "Merge",
            Self::CherryPick => "Cherry-pick",
            Self::Revert => "Revert",
            Self::Rebase => "Rebase",
        }
    }

    pub fn can_skip(self) -> bool {
        self != Self::Merge
    }
}

/// A multi-step git operation that stopped and waits for the user to continue, skip or abort it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitOperationState {
    pub kind: GitOperationKind,
    /// The commit that is being applied, if the operation stopped on one.
    pub current_commit: Option<SharedString>,
    /// Commits queued after the current one.
    pub remaining_commits: usize,
}

impl GitOperationState {
    pub fn detect(git_dir: &Path) -> Option<Self> {
        let read = |path: &Path| {
            std::fs::read_to_string(path)
                .ok()
                .map(|contents| contents.trim().to_string())
                .filter(|contents| !contents.is_empty())
        };
        let first_line =
            |name: &str| read(&git_dir.join(name)).and_then(|contents| first_line(&contents));

        // `git am` also uses rebase-apply, marking it with an `applying` file.
        let rebase_dir = [REBASE_MERGE_DIR, REBASE_APPLY_DIR]
            .into_iter()
            .map(|name| git_dir.join(name))
            .find(|dir| dir.is_dir() && !dir.join("applying").exists());
        if let Some(rebase_dir) = rebase_dir {
            let number = |names: [&str; 2]| {
                names
                    .iter()
                    .find_map(|name| read(&rebase_dir.join(name))?.parse::<usize>().ok())
            };
            let current = number(["msgnum", "next"]).unwrap_or_default();
            let total = number(["end", "last"]).unwrap_or_default();
            return Some(Self {
                kind: GitOperationKind::Rebase,
                current_commit: first_line("REBASE_HEAD"),
                remaining_commits: total.saturating_sub(current),
            });
        }

        if let Some(merge_head) = first_line("MERGE_HEAD") {
            return Some(Self {
                kind: GitOperationKind::Merge,
                current_commit: Some(merge_head),
                remaining_commits: 0,
            });
        }

        // The sequencer's todo list still includes the commit it stopped on.
        let todo = read(&git_dir.join(SEQUENCER_DIR).join("todo")).unwrap_or_default();
        let mut todo_verbs = todo
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .filter_map(|line| line.split_whitespace().next());
        let first_todo_kind = todo_verbs.next().and_then(|verb| match verb {
            "pick" | "p" => Some(GitOperationKind::CherryPick),
            "revert" => Some(GitOperationKind::Revert),
            _ => None,
        });
        let remaining_commits = todo_verbs.count();

        for (head, kind) in [
            ("CHERRY_PICK_HEAD", GitOperationKind::CherryPick),
            ("REVERT_HEAD", GitOperationKind::Revert),
        ] {
            if let Some(current_commit) = first_line(head) {
                return Some(Self {
                    kind,
                    current_commit: Some(current_commit),
                    remaining_commits,
                });
            }
        }

        first_todo_kind.map(|kind| Self {
            kind,
            current_commit: None,
            remaining_commits,
        })
    }
}

fn first_line(contents: &str) -> Option<SharedString> {
    contents
        .lines()
        .next()
        .map(|line| SharedString::from(line.trim().to_string()))
        .filter(|line| !line.is_empty())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum FastForward {
    #[default]
    Auto,
    Only,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ConflictPreference {
    Ours,
    Theirs,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct MergeOptions {
    pub source: String,
    pub fast_forward: FastForward,
    /// Stages the combined changes without recording a merge.
    pub squash: bool,
    pub commit: bool,
    pub signoff: bool,
    pub prefer: Option<ConflictPreference>,
    pub allow_unrelated_histories: bool,
}

impl MergeOptions {
    pub fn new(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            fast_forward: FastForward::Auto,
            squash: false,
            commit: true,
            signoff: false,
            prefer: None,
            allow_unrelated_histories: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ApplyCommitsOptions {
    pub commit: bool,
    /// Appends "(cherry picked from commit …)" to each message.
    pub record_origin: bool,
    /// The parent to diff against when applying a merge commit.
    pub mainline: Option<u32>,
    pub signoff: bool,
}

impl Default for ApplyCommitsOptions {
    fn default() -> Self {
        Self {
            commit: true,
            record_origin: false,
            mainline: None,
            signoff: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GitOperation {
    Merge(MergeOptions),
    CherryPick {
        commits: Vec<String>,
        options: ApplyCommitsOptions,
    },
    Revert {
        commits: Vec<String>,
        options: ApplyCommitsOptions,
    },
    Continue(GitOperationKind),
    Skip(GitOperationKind),
    Abort(GitOperationKind),
}

impl GitOperation {
    pub fn kind(&self) -> GitOperationKind {
        match self {
            Self::Merge(_) => GitOperationKind::Merge,
            Self::CherryPick { .. } => GitOperationKind::CherryPick,
            Self::Revert { .. } => GitOperationKind::Revert,
            Self::Continue(kind) | Self::Skip(kind) | Self::Abort(kind) => *kind,
        }
    }

    pub fn args(&self) -> Result<Vec<String>> {
        let mut args = vec![self.kind().subcommand().to_string()];
        match self {
            Self::Merge(options) => {
                validate_revision(&options.source)?;
                args.push("--no-edit".into());
                if options.squash {
                    args.push("--squash".into());
                } else {
                    match options.fast_forward {
                        FastForward::Auto => {}
                        FastForward::Only => args.push("--ff-only".into()),
                        FastForward::Never => args.push("--no-ff".into()),
                    }
                    if !options.commit {
                        args.push("--no-commit".into());
                    }
                }
                if options.signoff {
                    args.push("--signoff".into());
                }
                if let Some(prefer) = options.prefer {
                    args.push("-X".into());
                    args.push(
                        match prefer {
                            ConflictPreference::Ours => "ours",
                            ConflictPreference::Theirs => "theirs",
                        }
                        .into(),
                    );
                }
                if options.allow_unrelated_histories {
                    args.push("--allow-unrelated-histories".into());
                }
                args.push(options.source.clone());
            }
            Self::CherryPick { commits, options } | Self::Revert { commits, options } => {
                if commits.is_empty() {
                    bail!("no commits to apply");
                }
                for commit in commits {
                    validate_revision(commit)?;
                }
                if matches!(self, Self::Revert { .. }) {
                    args.push("--no-edit".into());
                }
                if !options.commit {
                    args.push("--no-commit".into());
                }
                if options.record_origin && matches!(self, Self::CherryPick { .. }) {
                    args.push("-x".into());
                }
                if let Some(mainline) = options.mainline {
                    args.push("-m".into());
                    args.push(mainline.to_string());
                }
                if options.signoff {
                    args.push("--signoff".into());
                }
                args.extend(commits.iter().cloned());
            }
            Self::Continue(_) => args.push("--continue".into()),
            Self::Skip(kind) => {
                if !kind.can_skip() {
                    bail!("a merge has no commits to skip");
                }
                args.push("--skip".into());
            }
            Self::Abort(_) => args.push("--abort".into()),
        }
        Ok(args)
    }

    /// The command as the user would type it, for previews.
    pub fn command_line(&self) -> Option<String> {
        let args = self.args().ok()?;
        Some(format!("git {}", args.join(" ")))
    }
}

/// Rejects revisions git would read as options.
pub fn validate_revision(revision: &str) -> Result<()> {
    if revision.trim().is_empty() {
        bail!("empty revision");
    }
    if revision.starts_with('-') {
        bail!("invalid revision: {revision}");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitOperationOutcome {
    /// The operation stopped and waits for the user, usually on conflicts.
    pub stopped: bool,
    pub output: RemoteCommandOutput,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum GitOperationPreviewRequest {
    Merge {
        source: String,
    },
    /// Revisions are single commits or `a..b` ranges, applied in the given order.
    CherryPick {
        revisions: Vec<String>,
        mainline: Option<u32>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PreviewCommit {
    pub sha: SharedString,
    pub subject: SharedString,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitOperationPreview {
    /// Commits a cherry-pick would apply, in order. Empty for merges.
    pub commits: Vec<PreviewCommit>,
    pub commit_count: usize,
    /// `None` when git can't forecast the result.
    pub changed_files: Option<usize>,
    /// `None` when git can't forecast the result.
    pub conflicted_paths: Option<Vec<String>>,
    pub can_fast_forward: bool,
    pub up_to_date: bool,
}

/// Parses `git merge-tree --write-tree --name-only --no-messages` output into the result tree and conflicted paths.
pub fn parse_merge_tree_output(output: &str) -> Option<(String, Vec<String>)> {
    let mut lines = output.lines();
    let tree = lines.next()?.trim().to_string();
    if tree.is_empty() {
        return None;
    }
    let conflicts = lines
        .take_while(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    Some((tree, conflicts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_merge_args() {
        let mut options = MergeOptions::new("feature");
        assert_eq!(
            GitOperation::Merge(options.clone())
                .command_line()
                .as_deref(),
            Some("git merge --no-edit feature")
        );

        options.fast_forward = FastForward::Never;
        options.commit = false;
        options.signoff = true;
        options.prefer = Some(ConflictPreference::Theirs);
        options.allow_unrelated_histories = true;
        assert_eq!(
            GitOperation::Merge(options.clone())
                .command_line()
                .as_deref(),
            Some(
                "git merge --no-edit --no-ff --no-commit --signoff -X theirs --allow-unrelated-histories feature"
            )
        );

        options.squash = true;
        assert_eq!(
            GitOperation::Merge(options).command_line().as_deref(),
            Some(
                "git merge --no-edit --squash --signoff -X theirs --allow-unrelated-histories feature"
            )
        );

        assert!(
            GitOperation::Merge(MergeOptions::new("--upload-pack=x"))
                .args()
                .is_err()
        );
        assert!(GitOperation::Merge(MergeOptions::new(" ")).args().is_err());
    }

    #[test]
    fn test_apply_commits_args() {
        let options = ApplyCommitsOptions {
            commit: false,
            record_origin: true,
            mainline: Some(1),
            signoff: true,
        };
        assert_eq!(
            GitOperation::CherryPick {
                commits: vec!["abc".into(), "def".into()],
                options: options.clone(),
            }
            .command_line()
            .as_deref(),
            Some("git cherry-pick --no-commit -x -m 1 --signoff abc def")
        );
        assert_eq!(
            GitOperation::Revert {
                commits: vec!["abc".into()],
                options,
            }
            .command_line()
            .as_deref(),
            Some("git revert --no-edit --no-commit -m 1 --signoff abc")
        );
        assert!(
            GitOperation::CherryPick {
                commits: Vec::new(),
                options: ApplyCommitsOptions::default(),
            }
            .args()
            .is_err()
        );
        assert!(
            GitOperation::CherryPick {
                commits: vec!["-n".into()],
                options: ApplyCommitsOptions::default(),
            }
            .args()
            .is_err()
        );
    }

    #[test]
    fn test_sequencer_args() {
        let kind = GitOperationKind::CherryPick;
        assert_eq!(
            GitOperation::Continue(kind).args().ok(),
            Some(vec!["cherry-pick".to_string(), "--continue".to_string()])
        );
        assert_eq!(
            GitOperation::Abort(GitOperationKind::Rebase).args().ok(),
            Some(vec!["rebase".to_string(), "--abort".to_string()])
        );
        assert!(GitOperation::Skip(GitOperationKind::Merge).args().is_err());
    }

    #[test]
    fn test_detect_operation_state() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path();
        assert_eq!(GitOperationState::detect(git_dir), None);

        std::fs::create_dir(git_dir.join(SEQUENCER_DIR)).unwrap();
        std::fs::write(
            git_dir.join(SEQUENCER_DIR).join("todo"),
            "pick 4d612de c1\npick 9bc18eb c2\npick 4ce9fc9 c3\n",
        )
        .unwrap();
        std::fs::write(git_dir.join("CHERRY_PICK_HEAD"), "4d612de\n").unwrap();
        assert_eq!(
            GitOperationState::detect(git_dir),
            Some(GitOperationState {
                kind: GitOperationKind::CherryPick,
                current_commit: Some("4d612de".into()),
                remaining_commits: 2,
            })
        );

        std::fs::remove_file(git_dir.join("CHERRY_PICK_HEAD")).unwrap();
        assert_eq!(
            GitOperationState::detect(git_dir),
            Some(GitOperationState {
                kind: GitOperationKind::CherryPick,
                current_commit: None,
                remaining_commits: 2,
            })
        );

        std::fs::write(git_dir.join("MERGE_HEAD"), "abc\n").unwrap();
        assert_eq!(
            GitOperationState::detect(git_dir).map(|state| state.kind),
            Some(GitOperationKind::Merge)
        );

        let rebase_dir = git_dir.join(REBASE_MERGE_DIR);
        std::fs::create_dir(&rebase_dir).unwrap();
        std::fs::write(rebase_dir.join("msgnum"), "2\n").unwrap();
        std::fs::write(rebase_dir.join("end"), "5\n").unwrap();
        std::fs::write(git_dir.join("REBASE_HEAD"), "def\n").unwrap();
        assert_eq!(
            GitOperationState::detect(git_dir),
            Some(GitOperationState {
                kind: GitOperationKind::Rebase,
                current_commit: Some("def".into()),
                remaining_commits: 3,
            })
        );
    }

    #[test]
    fn test_parse_merge_tree_output() {
        assert_eq!(
            parse_merge_tree_output("abc123\n"),
            Some(("abc123".to_string(), Vec::new()))
        );
        assert_eq!(
            parse_merge_tree_output("abc123\nsrc/a.rs\nb.txt\n\nAuto-merging\n"),
            Some((
                "abc123".to_string(),
                vec!["src/a.rs".to_string(), "b.txt".to_string()]
            ))
        );
        assert_eq!(parse_merge_tree_output(""), None);
    }
}
