#![warn(
	absolute_paths_not_starting_with_crate,
	future_incompatible,
	keyword_idents,
	macro_use_extern_crate,
	meta_variable_misuse,
	missing_abi,
	missing_copy_implementations,
	non_ascii_idents,
	nonstandard_style,
	noop_method_call,
	pointer_structural_match,
	private_in_public,
	rust_2018_idioms,
	unused_qualifications
)]
#![warn(clippy::pedantic)]
#![allow(clippy::let_underscore_drop)]

use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{anyhow, bail, ensure, Context as _, Result};

/// Manage disk mounting
#[derive(Debug, argh::FromArgs)]
struct Args {
	/// mount the specified disk
	#[argh(switch, short = 'm')]
	mount: bool,
	/// unmount the specified disk
	#[argh(switch, short = 'u')]
	unmount: bool,
	/// enter a subshell session within the specified disk
	///
	/// the disk will be unmounted on exit.
	#[argh(switch, short = 'c')]
	cd: bool,

	#[argh(positional)]
	disk: Disk,
}

enum Action {
	Mount,
	Unmount,
	Cd,
}

impl Action {
	pub fn from_bools(mount: bool, unmount: bool, cd: bool) -> Result<Action> {
		Ok(match (mount, unmount, cd) {
			(true, false, false) => Self::Mount,
			(false, true, false) => Self::Unmount,
			(false, false, true) => Self::Cd,
			(false, false, false) => bail!("no action specified. expected -m, -u, or -c."),
			_ => bail!("multiple actions specified. only one is allowed."),
		})
	}
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
enum Disk {
	Zdani,
	Sivydatni,
	Muhackiku,
	Barda,
}

impl Disk {
	fn as_repr(self) -> &'static str {
		match self {
			Self::Zdani => "zdani",
			Self::Sivydatni => "sivydatni",
			Self::Muhackiku => "muhackiku",
			Self::Barda => "barda",
		}
	}

	fn inner_filesystem(self) -> &'static str {
		match self {
			Self::Zdani | Self::Sivydatni | Self::Muhackiku | Self::Barda => "ext4",
		}
	}

	fn to_mountable(self) -> Mountable {
		match self {
			Self::Zdani => Mountable::Plain {
				uuid: "9972ca08-32d9-42da-9418-1afa4a7f6966",
			},
			Self::Sivydatni => Mountable::Encrypted {
				outer_uuid: "a02adf15-769d-4b61-9122-ddb3b3d1e7c2",
				inner_uuid: "ac80428f-f91d-4b99-9d40-c885d122be18",
			},
			Self::Muhackiku => Mountable::Encrypted {
				outer_uuid: "809dbaf9-4c95-4baf-890c-e6866dd1a913",
				inner_uuid: "e1258f59-cb99-4b6b-8bd7-513c66d64439",
			},
			Self::Barda => Mountable::Plain {
				uuid: "8f8ccfd3-aeae-4515-b081-3706561c64d4",
			},
		}
	}

	fn is_encrypted(self) -> bool {
		match self.to_mountable() {
			Mountable::Plain { .. } => false,
			Mountable::Encrypted { .. } => true,
		}
	}
}

#[derive(Debug, thiserror::Error)]
#[error("unknown disk {0:?}. valid disks are z (zdani), s (sivydatni), m (muhackiku), b (barda).")]
struct UnknownDisk(String);

impl FromStr for Disk {
	type Err = UnknownDisk;

	fn from_str(s: &str) -> Result<Self, UnknownDisk> {
		Ok(match s {
			"z" => Self::Zdani,
			"s" => Self::Sivydatni,
			"m" => Self::Muhackiku,
			"b" => Self::Barda,
			_ => return Err(UnknownDisk(s.to_owned())),
		})
	}
}

enum Mountable {
	Plain {
		uuid: &'static str,
	},
	Encrypted {
		outer_uuid: &'static str,
		inner_uuid: &'static str,
	},
}

fn dev_path_for_uuid(uuid: &str) -> Result<PathBuf> {
	let by_uuid = format!("/dev/disk/by-uuid/{uuid}");
	std::fs::canonicalize(by_uuid).context("getting canonical device for by-UUID symlink")
}

fn mount_path_for_name(name: &str) -> String {
	format!("/mnt/{name}")
}

fn opened_name_for_encrypted(uuid: &str, disk_name: &str) -> String {
	format!("{uuid}-{disk_name}")
}

struct MountReturn {
	mount_path: String,
	was_already_mounted: bool,
}

/// Returns the mount path, if successful.
fn mount(uuid: &str, disk_name: &str, filesystem: &str) -> Result<MountReturn> {
	use nix::mount::{mount, MsFlags};

	let mount_path = mount_path_for_name(disk_name);

	if !std::path::Path::try_exists(mount_path.as_ref())
		.context("verifying that mount path exists")?
	{
		eprintln!("mount path ({mount_path:?}) does not exist, trying to create it.");
		std::fs::create_dir_all(&mount_path).context("creating mount path")?;
	}

	let mount_res = mount(
		Some(&dev_path_for_uuid(uuid)?),
		mount_path.as_str(),
		Some(filesystem),
		MsFlags::MS_RELATIME | MsFlags::MS_LAZYTIME,
		None::<&str>,
	);
	let was_already_mounted = match mount_res {
		Err(nix::errno::Errno::EBUSY) => {
			eprintln!("mount returned EBUSY, assuming already mounted.");
			true
		}
		other => {
			other.context("making mount syscall")?;
			false
		}
	};

	Ok(MountReturn {
		mount_path,
		was_already_mounted,
	})
}

fn unmount(disk_name: &str) -> Result<()> {
	use nix::mount::umount;

	let mount_path = mount_path_for_name(disk_name);

	if std::path::Path::try_exists(mount_path.as_ref()).context("verifying that mount path exists")? {
		let umount_res = umount(mount_path.as_str());
		match umount_res {
			Err(nix::errno::Errno::EINVAL) => {
				eprintln!("umount returned EINVAL, assuming already unmounted.");
			}
			other => other.context("making umount syscall")?,
		}
	}

	Ok(())
}

fn open_encrypted(luks_uuid: &str, disk_name: &str) -> Result<()> {
	let code = std::process::Command::new("cryptsetup")
		.arg("open")
		.arg(dev_path_for_uuid(luks_uuid)?)
		.arg(opened_name_for_encrypted(luks_uuid, disk_name))
		.status()?;

	if code.success() {
		Ok(())
	} else {
		Err(anyhow!("cryptsetup exited with status {:?}", code.code()))
	}
}

fn close_encrypted(luks_uuid: &str, disk_name: &str) -> Result<()> {
	let code = std::process::Command::new("cryptsetup")
		.arg("close")
		.arg(opened_name_for_encrypted(luks_uuid, disk_name))
		.status()?;

	if code.success() {
		Ok(())
	} else {
		Err(anyhow!("cryptsetup exited with status {:?}", code.code()))
	}
}

fn do_mount(disk: Disk) -> Result<MountReturn> {
	let disk_name = disk.as_repr();
	let inner_filesystem = disk.inner_filesystem();
	let mountable = disk.to_mountable();

	match mountable {
		Mountable::Plain { uuid } => mount(uuid, disk_name, inner_filesystem).context("mounting"),
		Mountable::Encrypted {
			outer_uuid,
			inner_uuid,
		} => {
			open_encrypted(outer_uuid, disk_name).context("opening encrypted device")?;
			mount(inner_uuid, disk_name, inner_filesystem).context("mounting")
		}
	}
}

fn do_unmount(disk: Disk) -> Result<()> {
	let disk_name = disk.as_repr();
	let mountable = disk.to_mountable();

	match mountable {
		Mountable::Plain { .. } => {
			unmount(disk_name).context("unmounting")?;
		}
		Mountable::Encrypted {
			outer_uuid,
			inner_uuid: _,
		} => {
			unmount(disk_name).context("unmounting")?;
			close_encrypted(outer_uuid, disk_name).context("closing encrypted device")?;
		}
	}

	Ok(())
}

fn do_cd(disk: Disk) -> Result<()> {
	use std::os::unix::process::CommandExt as _;

	let MountReturn {
		mount_path,
		was_already_mounted,
	} = do_mount(disk)?;
	eprintln!("d: entering subshell. stay safe, friend.");
	let mut shell = std::process::Command::new("fish")
		.uid(nix::unistd::Uid::current().as_raw())
		.gid(nix::unistd::Gid::current().as_raw())
		.current_dir(mount_path)
		.args(["--private"].into_iter().filter(|_| disk.is_encrypted()))
		.spawn()
		.context("spawning sub-shell")?;
	shell.wait().context("waiting for sub-shell")?;
	if was_already_mounted {
		eprintln!("d: disk was already mounted; not unmounting.");
	} else {
		eprintln!("d: cleaning up; unmounting.");
		do_unmount(disk)?;
	}
	Ok(())
}

fn main() -> Result<()> {
	ensure!(
		nix::unistd::Uid::effective().is_root(),
		"must be run as root to (un)mount disks and open/close encryption"
	);

	let args: Args = argh::from_env();
	let action = Action::from_bools(args.mount, args.unmount, args.cd).context("reading action")?;

	match action {
		Action::Mount => {
			let MountReturn { mount_path, .. } = do_mount(args.disk)?;
			eprintln!("mounted {} at {mount_path:?}.", args.disk.as_repr());
		}
		Action::Unmount => {
			do_unmount(args.disk)?;
			eprintln!("unmounted {}.", args.disk.as_repr());
		}
		Action::Cd => {
			do_cd(args.disk)?;
		}
	}

	Ok(())
}
