#!/bin/bash
# Установщик overnet для Linux и macOS.
#
#   curl -fsSL https://raw.githubusercontent.com/ospab/overnet/master/scripts/install.sh | sudo bash
#   curl -fsSL …/install.sh | sudo bash -s -- --role relay
#
# Ставит бинарник в /opt/overnet (ссылка /usr/local/bin/overnet), конфиг кладёт
# в /etc/overnet/config.json, данные сервисов — в /var/lib/overnet. Повторный
# запуск обновляет бинарник и перезапускает сервисы overnet; конфиг не трогает.
set -e

GITHUB_REPO="ospab/overnet"
INSTALL_DIR="/opt/overnet"
BIN_LINK="/usr/local/bin/overnet"
CONFIG_DIR="/etc/overnet"
CONFIG_FILE="$CONFIG_DIR/config.json"
DATA_DIR="/var/lib/overnet"

usage() {
    cat <<'EOF'
Usage: install.sh [options]

  -v, --version TAG       install this release instead of the latest (e.g. v0.2.0)
  --config-url URL        download the network config (relays, reserved names) from URL
  --role ROLE             also run a systemd service; repeatable:
                            relay              relay (advertises --advertise, or this host's IP)
                            bootstrap          relay directory on 0.0.0.0:8080
                            gateway            SOCKS5 gateway on 127.0.0.1:9150
                            site:KIND          network site: name, search, files or mail
                            service:NAME:PORT  publish http://127.0.0.1:PORT as an .ov site,
                                               e.g. service:source:3000 for a Gitea
  --advertise HOST:PORT   address the relay reports to the directory
  --uninstall             stop overnet services and remove the binary (config and data stay)
  -y, --yes               no questions
  -h, --help              this help
EOF
}

echo "========================================================"
echo " overnet installer"
echo "========================================================"

TARGET_VERSION=""
CONFIG_URL=""
ADVERTISE=""
ROLES=()
ASSUME_YES=0
UNINSTALL=0
while [[ $# -gt 0 ]]; do
    case $1 in
        -v|--version)  TARGET_VERSION="$2"; shift 2 ;;
        --config-url)  CONFIG_URL="$2"; shift 2 ;;
        --role)        ROLES+=("$2"); shift 2 ;;
        --advertise)   ADVERTISE="$2"; shift 2 ;;
        --uninstall)   UNINSTALL=1; shift ;;
        -y|--yes)      ASSUME_YES=1; shift ;;
        -h|--help)     usage; exit 0 ;;
        *) echo "[error] Unknown option: $1"; usage; exit 1 ;;
    esac
done

if [ "$EUID" -ne 0 ]; then
    echo "[error] Root privileges required. Run with sudo."
    exit 1
fi

HAS_SYSTEMD=0
command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ] && HAS_SYSTEMD=1

overnet_units() {
    [ "$HAS_SYSTEMD" -eq 1 ] || return 0
    systemctl list-unit-files 'overnet-*.service' --no-legend 2>/dev/null | awk '{print $1}'
}

# -- Uninstall ----------------------------------------------------------

if [ "$UNINSTALL" -eq 1 ]; then
    for u in $(overnet_units); do
        echo "Stopping $u"
        systemctl disable --now "$u" >/dev/null 2>&1 || true
        rm -f "/etc/systemd/system/$u"
    done
    [ "$HAS_SYSTEMD" -eq 1 ] && systemctl daemon-reload
    rm -f "$BIN_LINK"
    rm -rf "$INSTALL_DIR"
    echo "overnet removed. Config ($CONFIG_DIR) and data ($DATA_DIR) were kept; delete them by hand if you don't need them."
    exit 0
fi

# -- Platform -----------------------------------------------------------

OS=$(uname -s)
case "$OS" in
    Linux)  OS="linux" ;;
    Darwin) OS="macos" ;;
    *) echo "[error] Unsupported system: $OS. On Windows use install.ps1."; exit 1 ;;
esac
ARCH=$(uname -m)
case "$ARCH" in
    x86_64|amd64)  ARCH="amd64" ;;
    aarch64|arm64) ARCH="arm64" ;;
    *) echo "[error] No overnet build for $ARCH yet. Build from source: cargo build --release -p overnet-cli"; exit 1 ;;
esac
echo "Platform: $OS/$ARCH"
if [ "$OS" = macos ] && [ "$ARCH" = amd64 ]; then
    echo "[error] No build for Intel Macs yet. Build from source: cargo build --release -p overnet-cli"; exit 1
fi

# -- Download -----------------------------------------------------------

if [ -n "$TARGET_VERSION" ]; then
    TAG="$TARGET_VERSION"
    [[ "$TAG" =~ ^v ]] || TAG="v$TAG"
else
    echo "Fetching latest release..."
    TAG=$(curl -fsSL "https://api.github.com/repos/${GITHUB_REPO}/releases/latest" 2>/dev/null \
        | grep '"tag_name":' | sed -E 's/.*"([^"]+)".*/\1/' || true)
    if [ -z "$TAG" ]; then
        echo "[error] Could not determine the latest release (is GitHub reachable from here?)."
        echo "        Pass a tag with --version, or see https://github.com/$GITHUB_REPO/releases"
        exit 1
    fi
fi

ARCHIVE="overnet-${OS}-${ARCH}.tar.gz"
URL="https://github.com/${GITHUB_REPO}/releases/download/${TAG}/${ARCHIVE}"
TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
echo "Downloading: $ARCHIVE ($TAG)"
# Полоса прогресса curl идёт в stderr; код ответа (-w) — в stdout.
HTTP_CODE=$(curl -L --progress-bar -w "%{http_code}" "$URL" -o "$TMP/$ARCHIVE")
if [ "$HTTP_CODE" != "200" ]; then
    echo "[error] Download failed (HTTP $HTTP_CODE): $URL"
    exit 1
fi
if command -v sha256sum >/dev/null 2>&1 && curl -fsSL "$URL.sha256" -o "$TMP/$ARCHIVE.sha256" 2>/dev/null; then
    (cd "$TMP" && sha256sum -c "$ARCHIVE.sha256" >/dev/null) || { echo "[error] Checksum mismatch for $ARCHIVE."; exit 1; }
    echo "Checksum OK."
fi
tar -xzf "$TMP/$ARCHIVE" -C "$TMP"

mkdir -p "$INSTALL_DIR" "$CONFIG_DIR" "$DATA_DIR"
# Подмена через rename: работающий бинарник не мешает.
install -m 755 "$TMP/overnet" "$INSTALL_DIR/overnet.new"
mv -f "$INSTALL_DIR/overnet.new" "$INSTALL_DIR/overnet"
[ -f "$TMP/config.example.json" ] && install -m 644 "$TMP/config.example.json" "$INSTALL_DIR/config.example.json"
ln -sf "$INSTALL_DIR/overnet" "$BIN_LINK"
echo "Installed: $BIN_LINK -> $INSTALL_DIR/overnet ($TAG)"

# -- Config -------------------------------------------------------------

FIRST_INSTALL=0
if [ ! -f "$CONFIG_FILE" ]; then
    FIRST_INSTALL=1
    if [ -n "$CONFIG_URL" ]; then
        echo "Downloading network config: $CONFIG_URL"
        curl -fsSL "$CONFIG_URL" -o "$CONFIG_FILE"
    else
        cp "$INSTALL_DIR/config.example.json" "$CONFIG_FILE"
    fi
    chmod 644 "$CONFIG_FILE"
    echo "Config: $CONFIG_FILE"
elif [ -n "$CONFIG_URL" ]; then
    echo "[notice] $CONFIG_FILE already exists; --config-url ignored."
fi

# -- Services -----------------------------------------------------------

write_unit() { # name, description, exec args
    local name="$1" desc="$2" args="$3"
    cat > "/etc/systemd/system/overnet-$name.service" <<EOF
[Unit]
Description=overnet: $desc
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
WorkingDirectory=$DATA_DIR
Environment=OVERNET_HOME=$DATA_DIR
ExecStart=$INSTALL_DIR/overnet $args
Restart=always
RestartSec=5
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
EOF
    echo "Service: overnet-$name"
}

if [ ${#ROLES[@]} -gt 0 ] && [ "$HAS_SYSTEMD" -eq 0 ]; then
    echo "[error] --role needs systemd. Run the commands from 'overnet help' yourself instead."
    exit 1
fi

for role in "${ROLES[@]}"; do
    case "$role" in
        relay)
            if [ -z "$ADVERTISE" ]; then
                IP=$(ip -4 route get 1.1.1.1 2>/dev/null | sed -nE 's/.* src ([0-9.]+).*/\1/p')
                [ -n "$IP" ] && ADVERTISE="$IP:4040"
                case "$IP" in
                    10.*|192.168.*|172.1[6-9].*|172.2[0-9].*|172.3[01].*|"")
                        echo "[warn] This host's address (${IP:-unknown}) is not public. Pass --advertise PUBLIC_IP:4040." ;;
                esac
            fi
            write_unit relay "relay" "relay --config $CONFIG_FILE${ADVERTISE:+ --advertise $ADVERTISE}"
            ;;
        bootstrap)
            write_unit bootstrap "relay directory" "bootstrap 0.0.0.0:8080"
            ;;
        gateway)
            write_unit gateway "SOCKS5 gateway" "gateway --config $CONFIG_FILE"
            ;;
        site:name|site:search|site:files|site:mail)
            kind="${role#site:}"
            mkdir -p "$DATA_DIR/sites/$kind"
            write_unit "$kind" "$kind.ov" "site $kind --config $CONFIG_FILE --data $DATA_DIR/sites/$kind"
            ;;
        service:*:*)
            rest="${role#service:}"; name="${rest%%:*}"; port="${rest#*:}"
            if ! [[ "$name" =~ ^[a-z0-9-]+$ && "$port" =~ ^[0-9]+$ ]]; then
                echo "[error] --role $role: expected service:NAME:PORT, e.g. service:source:3000"; exit 1
            fi
            key="$DATA_DIR/$name.key"
            if [ ! -f "$key" ]; then
                "$INSTALL_DIR/overnet" keygen "$key" >/dev/null
                chmod 600 "$key"
            fi
            write_unit "$name" "$name (http://127.0.0.1:$port)" "service --config $CONFIG_FILE --key $key --port 80=127.0.0.1:$port"
            echo "        address: $("$INSTALL_DIR/overnet" address "$key")  (key: $key — back it up)"
            ;;
        *)
            echo "[error] Unknown role: $role"; usage; exit 1 ;;
    esac
done

if [ "$HAS_SYSTEMD" -eq 1 ]; then
    systemctl daemon-reload
    for role in "${ROLES[@]}"; do
        case "$role" in
            relay|bootstrap|gateway) u="overnet-$role" ;;
            site:*) u="overnet-${role#site:}" ;;
            service:*) r="${role#service:}"; u="overnet-${r%%:*}" ;;
        esac
        systemctl enable "$u.service" >/dev/null 2>&1
    done
    # Обновление: перезапустить всё, что уже работало, на новом бинарнике.
    for u in $(overnet_units); do
        if systemctl is-enabled --quiet "$u" 2>/dev/null; then
            systemctl restart "$u" && echo "Started: $u"
        fi
    done
fi

# -- Next steps ---------------------------------------------------------

echo "--------------------------------------------------------"
echo "The network's relays and service addresses are built in; $CONFIG_FILE"
echo "only needs changes for a network of your own."
for role in "${ROLES[@]}"; do
    [ "$role" = relay ] && echo "Relay line for client configs: journalctl -u overnet-relay | grep -m1 @"
done
[ ${#ROLES[@]} -gt 0 ] && echo "Logs: journalctl -u 'overnet-*' -f"
echo "Browse: overnet browser    Help: overnet help"
echo "--------------------------------------------------------"
