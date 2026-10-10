set -euxo pipefail

id gamer || useradd --uid 1100 --user-group --create-home --shell /bin/bash gamer
usermod -aG input,video,render,audio gamer
loginctl enable-linger gamer

echo uinput > /etc/modules-load.d/uinput.conf
modprobe uinput
echo 'KERNEL=="uinput", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"' > /etc/udev/rules.d/60-uinput.rules
udevadm control --reload-rules
udevadm trigger --name-match=uinput

# Steam and its games are content, not state: they live in the cache.
mkdir -p /cache/steam /home/gamer/.local/share
chown gamer:gamer /cache/steam /home/gamer/.local /home/gamer/.local/share
ln -sfn /cache/steam /home/gamer/.local/share/Steam

# nvidia-smi prints "00000000:BB:DD.F" in hex, Xorg wants "PCI:B:D:F" in decimal.
IFS=':.' read -r _ bus dev fn <<< "$(nvidia-smi --query-gpu=pci.bus_id --format=csv,noheader | head -1)"
xorg_bus_id=$(printf 'PCI:%d:%d:%d' "0x$bus" "0x$dev" "0x$fn")

# Datacenter GPUs have no display output: an empty initial configuration with a virtual screen
# gives Xorg (and NvFBC) a framebuffer. MetaModes is required, the driver otherwise picks 1024x768.
cat > /etc/X11/kg-xorg.conf <<XORG
Section "ServerLayout"
    Identifier "Layout0"
    Screen 0 "Screen0"
EndSection

Section "ServerFlags"
    Option "AutoAddGPU" "false"
EndSection

Section "Device"
    Identifier "Device0"
    Driver "nvidia"
    BusID "$xorg_bus_id"
    Option "AllowEmptyInitialConfiguration" "True"
    Option "HardDPMS" "false"
EndSection

Section "Screen"
    Identifier "Screen0"
    Device "Device0"
    DefaultDepth 24
    Option "MetaModes" "${SCREEN_WIDTH}x${SCREEN_HEIGHT} +0+0"
    SubSection "Display"
        Depth 24
        Virtual $SCREEN_WIDTH $SCREEN_HEIGHT
    EndSubSection
EndSection
XORG

# No sound card on the VM: a null sink gives games and Kyber's capture an output.
pw_conf=/home/gamer/.config/pipewire/pipewire.conf.d
mkdir -p "$pw_conf"
cat > "$pw_conf/kg-null-sink.conf" <<'PW'
context.objects = [
  { factory = adapter
    args = {
      factory.name     = support.null-audio-sink
      node.name        = "kg-sink"
      node.description = "kyber-gaming virtual output"
      media.class      = Audio/Sink
      audio.position   = [ FL FR ]
      priority.session = 2000
    }
  }
]
PW
chown -R gamer:gamer /home/gamer/.config

user_env='Environment=HOME=/home/gamer DISPLAY=:0 XDG_RUNTIME_DIR=/run/user/1100 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1100/bus'

cat > /etc/systemd/system/kg-xorg.service <<UNIT
[Unit]
Description=Headless Xorg on the NVIDIA GPU

[Service]
ExecStart=/usr/lib/xorg/Xorg :0 -config /etc/X11/kg-xorg.conf -nolisten tcp -noreset vt7
ExecStartPost=/bin/sh -c 'until [ -S /tmp/.X11-unix/X0 ]; do sleep 0.5; done; DISPLAY=:0 xhost +si:localuser:gamer'
Restart=on-failure
UNIT

cat > /etc/systemd/system/kg-openbox.service <<UNIT
[Unit]
Description=Openbox on :0
Requires=kg-xorg.service
After=kg-xorg.service user@1100.service

[Service]
User=gamer
$user_env
ExecStart=/usr/bin/openbox
Restart=on-failure
UNIT

cat > /etc/systemd/system/kg-steam.service <<UNIT
[Unit]
Description=Steam on :0
Requires=kg-xorg.service
After=kg-openbox.service user@1100.service

[Service]
User=gamer
WorkingDirectory=/home/gamer
$user_env
ExecStart=/usr/bin/steam
Restart=on-failure
RestartSec=5
UNIT

systemctl daemon-reload
# gamer's user manager started at enable-linger, before PipeWire's user units were usable.
systemctl restart user@1100.service
# reset-failed also zeroes NRestarts, which the check below relies on.
systemctl reset-failed kg-xorg.service kg-openbox.service kg-steam.service 2>/dev/null || true
systemctl restart kg-xorg.service kg-openbox.service kg-steam.service

# A unit stuck in a restart loop still reads "active" between attempts: require a quiet stretch.
sleep 10
for unit in kg-xorg kg-openbox kg-steam; do
  if ! systemctl is-active --quiet "$unit" || [ "$(systemctl show -p NRestarts --value "$unit")" != 0 ]; then
    journalctl -u "$unit" --no-pager -n 20 >&2
    echo "$unit is not running steadily" >&2
    exit 1
  fi
done
