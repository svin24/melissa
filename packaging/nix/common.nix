# Where the Nix packaging finds Melissa's host-agnostic files (systemd unit, tmpfiles, k3s manifests,
# polkit rule, helper scripts). Paths below this root mirror their install paths
# (`usr/lib/systemd/system/melissa-queen.service`, ...).
#
# These files are shared with the container build and must not be copied into packaging/nix.
# When they move to packaging/common, change `root` to `../common`; nothing else changes.
{
  root = ../../image/rootfs;
}
