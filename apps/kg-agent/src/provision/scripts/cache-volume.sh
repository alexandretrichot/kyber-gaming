set -euxo pipefail

mkdir -p /cache
mountpoint -q /cache && exit 0

# Provider agnostic: the cache is the only whole disk with nothing mounted anywhere in its tree.
candidates=()
while read -r disk type; do
  [ "$type" = disk ] || continue
  lsblk -nrpo MOUNTPOINTS "$disk" | grep -q . || candidates+=("$disk")
done < <(lsblk -dnrpo NAME,TYPE)

case ${#candidates[@]} in
  0) echo "no cache volume attached, /cache stays on the ephemeral boot disk" ;;
  1)
    blkid "${candidates[0]}" || mkfs.ext4 -q -L kg-cache "${candidates[0]}"
    mount "${candidates[0]}" /cache
    ;;
  *) echo "several unmounted disks (${candidates[*]}), cannot tell which one is the cache" >&2; exit 1 ;;
esac
