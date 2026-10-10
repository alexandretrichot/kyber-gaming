set -euxo pipefail

kyber=/opt/kg/kyber
[ -x "$kyber/bin/kycontroller" ] || { echo "Kyber runtime missing under $kyber" >&2; exit 1; }

config=/var/lib/kg-agent/kyber_config.toml
mkdir -p /var/lib/kg-agent
sed \
  -e "s|^#\?port = .*|port = $KYBER_PORT|" \
  -e "s|{ username = \"Default user\", password = \"[0-9a-f]*\" }|{ username = \"gamer\", password = \"$KYBER_PASSWORD_SHA256\" }|" \
  "$kyber/bin/kyber_config.toml" > "$config"
grep -q "$KYBER_PASSWORD_SHA256" "$config" || { echo "failed to set Kyber credentials" >&2; exit 1; }
chmod 0644 "$config"

cat > /etc/systemd/system/kg-kyber.service <<UNIT
[Unit]
Description=Kyber controller
Requires=kg-xorg.service
After=kg-xorg.service user@1100.service

[Service]
User=gamer
WorkingDirectory=/home/gamer
Environment=HOME=/home/gamer DISPLAY=:0 XDG_RUNTIME_DIR=/run/user/1100 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1100/bus KYBER_CONFIG=$config
ExecStart=$kyber/run_kycontroller.sh
Restart=on-failure
RestartSec=3
UNIT

systemctl daemon-reload
systemctl reset-failed kg-kyber.service 2>/dev/null || true
systemctl restart kg-kyber.service
for _ in $(seq 30); do
  ss -lnt "sport = :$KYBER_PORT" | grep -q LISTEN && exit 0
  sleep 1
done
echo "kycontroller is not listening on $KYBER_PORT" >&2
journalctl -u kg-kyber --no-pager -n 30 >&2
exit 1
