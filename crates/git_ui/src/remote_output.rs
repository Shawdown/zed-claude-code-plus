use anyhow::Context as _;

use git::{
    operation::GitOperation,
    repository::{Remote, RemoteCommandOutput},
};
use ui::SharedString;
use util::ResultExt as _;

const PULL_REQUEST_HINTS: &[(&str, &str)] = &[
    // GitHub: "Create a pull request for 'branch' on GitHub by visiting:"
    ("Create a pull request", "Create Pull Request"),
    // Bitbucket: "Create pull request for branch:"
    ("Create pull request", "Create Pull Request"),
    // GitLab: "To create a merge request for branch, visit:"
    ("create a merge request", "Create Merge Request"),
    // GitLab: "View merge request for branch:"
    ("View merge request", "View Merge Request"),
];

#[derive(Clone)]
pub enum RemoteAction {
    Fetch(Option<Remote>),
    Pull(Remote),
    Push(SharedString, Remote),
    Operation {
        operation: GitOperation,
        /// The operation paused, usually on conflicts.
        stopped: bool,
    },
}

impl RemoteAction {
    pub fn name(&self) -> &'static str {
        match self {
            RemoteAction::Fetch(_) => "fetch",
            RemoteAction::Pull(_) => "pull",
            RemoteAction::Push(_, _) => "push",
            RemoteAction::Operation { operation, .. } => operation.kind().subcommand(),
        }
    }
}

pub enum SuccessStyle {
    Toast,
    ToastWithLog { output: RemoteCommandOutput },
    PushPrLink { label: &'static str, url: String },
}

pub struct SuccessMessage {
    pub message: String,
    pub style: SuccessStyle,
}

fn extract_pull_request_link(output: &RemoteCommandOutput) -> Option<(&'static str, String)> {
    let mut pending_label: Option<&'static str> = None;

    for line in output.stderr.lines() {
        let Some(remote_line) = line.trim_start().strip_prefix("remote:") else {
            pending_label = None;
            continue;
        };

        if let Some((_, label)) = PULL_REQUEST_HINTS
            .iter()
            .find(|(hint, _)| remote_line.contains(hint))
        {
            pending_label = Some(label);
        }

        if let Some(url) = extract_url(remote_line)
            && let Some(label) = pending_label
        {
            return Some((label, url));
        }
    }

    None
}

fn extract_url(line: &str) -> Option<String> {
    let http_index = line.find("https://").or_else(|| line.find("http://"))?;
    let url = line[http_index..]
        .split_whitespace()
        .next()?
        .trim_end_matches(|character| matches!(character, ',' | '.' | ')' | ']' | '>'));

    Some(url.to_string())
}

pub fn format_output(action: &RemoteAction, output: RemoteCommandOutput) -> SuccessMessage {
    match action {
        RemoteAction::Operation { operation, stopped } => {
            format_operation_output(operation, *stopped, output)
        }
        RemoteAction::Fetch(remote) => {
            if output.stderr.is_empty() {
                SuccessMessage {
                    message: "Fetch: Already up to date".into(),
                    style: SuccessStyle::Toast,
                }
            } else {
                let message = match remote {
                    Some(remote) => format!("Synchronized with {}", remote.name),
                    None => "Synchronized with remotes".into(),
                };
                SuccessMessage {
                    message,
                    style: SuccessStyle::ToastWithLog { output },
                }
            }
        }
        RemoteAction::Pull(remote_ref) => {
            let get_changes = |output: &RemoteCommandOutput| -> anyhow::Result<u32> {
                let last_line = output
                    .stdout
                    .lines()
                    .last()
                    .context("Failed to get last line of output")?
                    .trim();

                let files_changed = last_line
                    .split_whitespace()
                    .next()
                    .context("Failed to get first word of last line")?
                    .parse()?;

                Ok(files_changed)
            };
            if output.stdout.ends_with("Already up to date.\n") {
                SuccessMessage {
                    message: "Pull: Already up to date".into(),
                    style: SuccessStyle::Toast,
                }
            } else if output.stdout.starts_with("Updating") {
                let files_changed = get_changes(&output).log_err();
                let message = if let Some(files_changed) = files_changed {
                    format!(
                        "Received {} file change{} from {}",
                        files_changed,
                        if files_changed == 1 { "" } else { "s" },
                        remote_ref.name
                    )
                } else {
                    format!("Fast forwarded from {}", remote_ref.name)
                };
                SuccessMessage {
                    message,
                    style: SuccessStyle::ToastWithLog { output },
                }
            } else if output.stdout.starts_with("Merge") {
                let files_changed = get_changes(&output).log_err();
                let message = if let Some(files_changed) = files_changed {
                    format!(
                        "Merged {} file change{} from {}",
                        files_changed,
                        if files_changed == 1 { "" } else { "s" },
                        remote_ref.name
                    )
                } else {
                    format!("Merged from {}", remote_ref.name)
                };
                SuccessMessage {
                    message,
                    style: SuccessStyle::ToastWithLog { output },
                }
            } else if output.stdout.contains("Successfully rebased") {
                SuccessMessage {
                    message: format!("Successfully rebased from {}", remote_ref.name),
                    style: SuccessStyle::ToastWithLog { output },
                }
            } else {
                SuccessMessage {
                    message: format!("Successfully pulled from {}", remote_ref.name),
                    style: SuccessStyle::ToastWithLog { output },
                }
            }
        }
        RemoteAction::Push(branch_name, remote_ref) => {
            if output.stderr.ends_with("Everything up-to-date\n") {
                SuccessMessage {
                    message: "Push: Everything is up-to-date".to_string(),
                    style: SuccessStyle::Toast,
                }
            } else if let Some((label, url)) = extract_pull_request_link(&output) {
                SuccessMessage {
                    message: format!("Pushed {} to {}", branch_name, remote_ref.name),
                    style: SuccessStyle::PushPrLink { label, url },
                }
            } else {
                SuccessMessage {
                    message: format!("Pushed {} to {}", branch_name, remote_ref.name),
                    style: SuccessStyle::ToastWithLog { output },
                }
            }
        }
    }
}

fn describe_commits(commits: &[String]) -> String {
    match commits {
        [commit] if !commit.contains("..") => commit.chars().take(7).collect(),
        _ if commits.iter().any(|commit| commit.contains("..")) => "commits".into(),
        _ => format!("{} commits", commits.len()),
    }
}

fn format_operation_output(
    operation: &GitOperation,
    stopped: bool,
    output: RemoteCommandOutput,
) -> SuccessMessage {
    let label = operation.kind().label();
    let message = if stopped {
        format!("{label} paused: resolve conflicts, then continue")
    } else {
        match operation {
            GitOperation::Merge(options) => {
                if output.stdout.contains("Already up to date") {
                    return SuccessMessage {
                        message: "Merge: Already up to date".into(),
                        style: SuccessStyle::Toast,
                    };
                } else if options.squash {
                    format!("Squashed {}: review and commit", options.source)
                } else if output.stdout.starts_with("Updating") {
                    format!("Fast-forwarded to {}", options.source)
                } else if !options.commit {
                    format!("Merged {}: review and commit", options.source)
                } else {
                    format!("Merged {}", options.source)
                }
            }
            GitOperation::CherryPick { commits, options } => {
                if options.commit {
                    format!("Cherry-picked {}", describe_commits(commits))
                } else {
                    format!("Applied {}: review and commit", describe_commits(commits))
                }
            }
            GitOperation::Revert { commits, options } => {
                if options.commit {
                    format!("Reverted {}", describe_commits(commits))
                } else {
                    format!("Reverted {}: review and commit", describe_commits(commits))
                }
            }
            GitOperation::Continue(_) => format!("{label} completed"),
            GitOperation::Skip(_) => "Skipped commit".into(),
            GitOperation::Abort(_) => format!("{label} aborted"),
        }
    };
    SuccessMessage {
        message,
        style: SuccessStyle::ToastWithLog { output },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git::operation::{ApplyCommitsOptions, GitOperationKind, MergeOptions};
    use indoc::indoc;

    #[test]
    fn test_operation_messages() {
        let message = |operation: GitOperation, stopped: bool, stdout: &str| {
            format_output(
                &RemoteAction::Operation { operation, stopped },
                RemoteCommandOutput {
                    stdout: stdout.into(),
                    stderr: String::new(),
                },
            )
            .message
        };
        let merge = |configure: fn(&mut MergeOptions)| {
            let mut options = MergeOptions::new("feature");
            configure(&mut options);
            GitOperation::Merge(options)
        };
        let cherry_pick = |commits: &[&str], commit: bool| GitOperation::CherryPick {
            commits: commits.iter().map(ToString::to_string).collect(),
            options: ApplyCommitsOptions {
                commit,
                ..ApplyCommitsOptions::default()
            },
        };

        let up_to_date = format_output(
            &RemoteAction::Operation {
                operation: merge(|_| {}),
                stopped: false,
            },
            RemoteCommandOutput {
                stdout: "Already up to date.\n".into(),
                stderr: String::new(),
            },
        );
        assert_eq!(up_to_date.message, "Merge: Already up to date");
        assert!(matches!(up_to_date.style, SuccessStyle::Toast));

        assert_eq!(
            message(merge(|_| {}), false, "Updating abc..def\nFast-forward\n"),
            "Fast-forwarded to feature"
        );
        assert_eq!(
            message(merge(|_| {}), false, "Merge made by the 'ort' strategy.\n"),
            "Merged feature"
        );
        assert_eq!(
            message(merge(|options| options.commit = false), false, ""),
            "Merged feature: review and commit"
        );
        assert_eq!(
            message(
                merge(|options| options.squash = true),
                false,
                "Updating abc..def\nSquash commit -- not updating HEAD\n"
            ),
            "Squashed feature: review and commit"
        );
        assert_eq!(
            message(merge(|_| {}), true, "CONFLICT (content)"),
            "Merge paused: resolve conflicts, then continue"
        );
        assert_eq!(
            message(cherry_pick(&["0123456789abcdef"], true), false, ""),
            "Cherry-picked 0123456"
        );
        assert_eq!(
            message(cherry_pick(&["abc", "def"], false), false, ""),
            "Applied 2 commits: review and commit"
        );
        assert_eq!(
            message(cherry_pick(&["HEAD..feature"], true), false, ""),
            "Cherry-picked commits"
        );
        assert_eq!(
            message(GitOperation::Abort(GitOperationKind::CherryPick), false, ""),
            "Cherry-pick aborted"
        );
    }

    #[test]
    fn test_push_new_branch_pull_request() {
        let action = RemoteAction::Push(
            SharedString::new_static("test_branch"),
            Remote {
                name: SharedString::new_static("test_remote"),
            },
        );

        let output = RemoteCommandOutput {
            stdout: String::new(),
            stderr: indoc! { "
                Total 0 (delta 0), reused 0 (delta 0), pack-reused 0 (from 0)
                remote:
                remote: Create a pull request for 'test' on GitHub by visiting:
                remote:      https://example.com/test/test/pull/new/test
                remote:
                To example.com:test/test.git
                 * [new branch]      test -> test
                "}
            .to_string(),
        };

        let msg = format_output(&action, output);
        if let SuccessStyle::PushPrLink { label, url } = msg.style {
            assert_eq!(msg.message, "Pushed test_branch to test_remote");
            assert_eq!(label, "Create Pull Request");
            assert_eq!(url, "https://example.com/test/test/pull/new/test");
        } else {
            panic!("Expected PushPrLink variant");
        }
    }

    #[test]
    fn test_push_new_branch_merge_request() {
        let action = RemoteAction::Push(
            SharedString::new_static("test_branch"),
            Remote {
                name: SharedString::new_static("test_remote"),
            },
        );

        let output = RemoteCommandOutput {
            stdout: String::new(),
            stderr: indoc! {"
                Total 0 (delta 0), reused 0 (delta 0), pack-reused 0 (from 0)
                remote:
                remote: To create a merge request for test, visit:
                remote:   https://example.com/test/test/-/merge_requests/new?merge_request%5Bsource_branch%5D=test
                remote:
                To example.com:test/test.git
                 * [new branch]      test -> test
                "}
            .to_string()
            };

        let msg = format_output(&action, output);

        if let SuccessStyle::PushPrLink { label, url } = msg.style {
            assert_eq!(msg.message, "Pushed test_branch to test_remote");
            assert_eq!(label, "Create Merge Request");
            assert_eq!(
                url,
                "https://example.com/test/test/-/merge_requests/new?merge_request%5Bsource_branch%5D=test"
            )
        } else {
            panic!("Expected PushPrLink variant")
        }
    }

    #[test]
    fn test_push_new_branch_bitbucket_pull_request() {
        let output = RemoteCommandOutput {
            stdout: String::new(),
            stderr: indoc! {"
                remote:
                remote: Create pull request for test:
                remote:   https://bitbucket.example.com/projects/TEST/repos/test/pull-requests?create&sourceBranch=refs/heads/test
                "}
            .to_string(),
        };

        assert_eq!(
            extract_pull_request_link(&output),
            Some((
                "Create Pull Request",
                "https://bitbucket.example.com/projects/TEST/repos/test/pull-requests?create&sourceBranch=refs/heads/test".to_string()
            ))
        );
    }

    #[test]
    fn test_push_branch_existing_merge_request() {
        let action = RemoteAction::Push(
            SharedString::new_static("test_branch"),
            Remote {
                name: SharedString::new_static("test_remote"),
            },
        );

        let output = RemoteCommandOutput {
            stdout: String::new(),
            // Include an unrelated URL outside of the `remote:` lines, in this
            // case, an OpenSSH warning, to ensure that it is not mistaken for
            // the merge request link.
            stderr: indoc! {"
                ** WARNING: connection is not using a post-quantum key exchange algorithm.
                ** This session may be vulnerable to \"store now, decrypt later\" attacks.
                ** The server may need to be upgraded. See https://openssh.com/pq.html
                Total 0 (delta 0), reused 0 (delta 0), pack-reused 0 (from 0)
                remote:
                remote: View merge request for test:
                remote:    https://example.com/test/test/-/merge_requests/99999
                remote:
                To example.com:test/test.git
                    + 80bd3c83be...e03d499d2e test -> test
                "}
            .to_string(),
        };

        let msg = format_output(&action, output);

        if let SuccessStyle::PushPrLink { label, url } = msg.style {
            assert_eq!(msg.message, "Pushed test_branch to test_remote");
            assert_eq!(label, "View Merge Request");
            assert_eq!(url, "https://example.com/test/test/-/merge_requests/99999");
        } else {
            panic!("Expected PushPrLink variant")
        }
    }

    #[test]
    fn test_push_new_branch_no_link() {
        let action = RemoteAction::Push(
            SharedString::new_static("test_branch"),
            Remote {
                name: SharedString::new_static("test_remote"),
            },
        );

        let output = RemoteCommandOutput {
            stdout: String::new(),
            stderr: indoc! { "
                To http://example.com/test/test.git
                 * [new branch]      test -> test
                ",
            }
            .to_string(),
        };

        let msg = format_output(&action, output);

        if let SuccessStyle::ToastWithLog { output } = &msg.style {
            assert_eq!(
                output.stderr,
                "To http://example.com/test/test.git\n * [new branch]      test -> test\n"
            );
            assert_eq!(extract_pull_request_link(output), None);
        } else {
            panic!("Expected ToastWithLog variant");
        }
    }
}
