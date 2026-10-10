set -euxo pipefail
export DEBIAN_FRONTEND=noninteractive

# The GPU image ships a compute driver. Steam/Proton need the GL/Vulkan userspace (64 and 32 bit),
# Xorg needs the DDX, Kyber needs NvFBC + NVENC. Everything is pinned to the installed driver version:
# a newer candidate would not match the loaded kernel module.
driver_version=$(nvidia-smi --query-gpu=driver_version --format=csv,noheader | head -1)
major=${driver_version%%.*}
suffix=
for s in -server ""; do
  if dpkg-query -W "libnvidia-compute-${major}${s}" >/dev/null 2>&1; then suffix=$s; break; fi
done
dpkg-query -W "libnvidia-compute-${major}${suffix}" >/dev/null || {
  echo "NVIDIA driver $driver_version is not installed from Ubuntu packages, cannot pin userspace libs" >&2
  exit 1
}
pkg_version=$(dpkg-query -W -f='${Version}' "libnvidia-compute-${major}${suffix}")
pinned() { for p in "$@"; do echo "${p}${major}${suffix}=${pkg_version}"; done; }
pinned_i386() { for p in "$@"; do echo "${p}${major}${suffix}:i386=${pkg_version}"; done; }

dpkg --add-architecture i386
apt-get update
# shellcheck disable=SC2046 # the package lists are meant to be split
apt-get install -y --no-install-recommends \
  $(pinned libnvidia-gl- libnvidia-fbc1- libnvidia-encode- libnvidia-decode- libnvidia-cfg1- xserver-xorg-video-nvidia-) \
  $(pinned_i386 libnvidia-gl- libnvidia-compute-) \
  libvulkan1 libvulkan1:i386 vulkan-tools \
  xserver-xorg-core xserver-xorg-input-libinput x11-xserver-utils xauth \
  openbox xterm dbus-user-session \
  pipewire pipewire-pulse wireplumber pulseaudio-utils

# Valve's own package: the Steam license is accepted by the player in the Steam UI, not preseeded.
if ! dpkg-query -W steam-launcher >/dev/null 2>&1; then
  curl -fsSL -o /tmp/steam.deb https://cdn.akamai.steamstatic.com/client/installer/steam.deb
  apt-get install -y /tmp/steam.deb
  rm /tmp/steam.deb
  apt-get update
fi
apt-get install -y steam-libs-amd64 steam-libs-i386:i386 libc6:i386 libgl1:i386 libegl1:i386 libgbm1:i386
