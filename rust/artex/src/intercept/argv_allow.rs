//! Always-allow argv denylist — pure slice of `intercept/alwaysallow_denylist.go`.
//!
//! An argv allow rule for one of these programs runs arbitrary code or causes
//! unbounded damage, so it is refused. The list is code-owned on purpose.

use crate::db::error::{Error, Result};

const DENYLIST_GROUPS: &[&str] = &[
    "sh bash zsh dash ksh fish csh tcsh ash mksh rbash pwsh powershell python pypy ipython perl ruby irb node deno bun php lua luajit rscript tclsh wish expect awk gawk mawk nawk sed jshell osascript",
    "env command builtin exec eval source . nohup timeout nice ionice stdbuf setsid chroot unshare nsenter systemd-run su sudo doas pkexec runuser setpriv sg watch busybox toybox xargs parallel make gmake mapfile trap script find ssh scp sftp rsync socat ncat nc netcat flock strace ltrace gdb taskset chrt unbuffer rlwrap firejail bwrap proot proxychains torsocks tar zip vim vi nvim emacs less more man",
    "npm npx pnpm yarn bunx corepack pip pipx uv uvx poetry conda composer gem bundle bundler rake cargo go mvn gradle dotnet apt apt-get dpkg dnf yum rpm pacman apk brew snap flatpak",
    "docker docker-compose podman nerdctl kubectl helm ansible ansible-playbook terraform pulumi vagrant",
    "rm dd mkfs mkswap wipefs fdisk sfdisk parted shred truncate chmod chown kill pkill killall systemctl service iptables ip6tables nft ufw mount umount swapoff shutdown reboot poweroff halt init telinit crontab insmod rmmod modprobe",
];

fn denylisted(name: &str) -> bool {
    DENYLIST_GROUPS
        .iter()
        .flat_map(|g| g.split_whitespace())
        .any(|n| n == name)
}

/// `argvProgramName`: basename, lowercase, trailing version digits/dots removed
/// (but only after a letter), and every `mkfs.*` reduced to `mkfs`.
pub fn argv_program_name(tok: &str) -> String {
    let base = tok.rsplit('/').next().unwrap_or(tok);
    let name = base.to_lowercase();
    if name.starts_with("mkfs.") {
        return "mkfs".to_string();
    }
    let mut end = name.len();
    while end > 0 {
        let c = name.as_bytes()[end - 1];
        if c == b'.' || c.is_ascii_digit() {
            end -= 1;
        } else {
            break;
        }
    }
    if end > 0 && end < name.len() {
        name[..end].to_string()
    } else {
        name
    }
}

/// `ArgvAllowDenied`: whether an argv allow rule naming `program` is refused.
pub fn argv_allow_denied(program: &str) -> bool {
    denylisted(&argv_program_name(program))
}

/// `CheckArgvAllow`: the error shown for a denylisted first token (None = allowed).
pub fn check_argv_allow(tokens: &[String]) -> Result<()> {
    if let Some(first) = tokens.first() {
        if argv_allow_denied(first) {
            return Err(Error::msg(format!(
                "program {first:?} cannot be always allowed: it can run arbitrary code or cause unbounded damage whatever its arguments"
            )));
        }
    }
    Ok(())
}
