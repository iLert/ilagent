#!/usr/bin/env bash
set -e
if [ -z "${DEBUG}" ]; then
  set +o xtrace
else
  set -o xtrace
fi

curl_fetch_headers() {
  curl --silent --show-error --location --head --connect-timeout 10 --max-time 30 "$1"
}

curl_download_file() {
  local url="$1"
  local output_path="$2"
  local download_connect_timeout=10
  local download_max_time=600
  local download_retry_count=2
  local download_retry_delay=2
  local download_retry_max_time=620

  if [ -t 2 ]; then
    curl --show-error --location --fail --connect-timeout "$download_connect_timeout" --max-time "$download_max_time" --retry "$download_retry_count" --retry-delay "$download_retry_delay" --retry-max-time "$download_retry_max_time" "$url" --output "$output_path"
  else
    curl --silent --show-error --location --fail --connect-timeout "$download_connect_timeout" --max-time "$download_max_time" --retry "$download_retry_count" --retry-delay "$download_retry_delay" --retry-max-time "$download_retry_max_time" "$url" --output "$output_path"
  fi
}

VERSION=$(curl_fetch_headers "https://github.com/iLert/ilagent/releases/latest" | grep -i '^location:' | sed 's|.*/||' | tr -d '\r\n')
if [ -z "$VERSION" ]; then
  echo "Failed to determine latest release version."
  exit 1
fi
echo "Installing ilagent version ${VERSION}"

# Prompt user to run a command with sudo; show exact command first
run_with_sudo_prompt() {
  local cmd="$1"
  local reason="$2"
  echo ""
  if [ -n "$reason" ]; then
    echo "$reason"
  fi
  echo "This usually needs administrator privileges."
  echo "The installer can continue by running:"
  echo "  sudo $cmd"
  read -r -p "Run with sudo? [y/N] " answer </dev/tty
  case "$answer" in
    [yY]|[yY][eE][sS])
      eval "sudo $cmd"
      ;;
    *)
      echo "Aborted."
      exit 1
      ;;
  esac
}

# Move binary to install path, escalating to sudo if needed
install_binary() {
  local tmp_file="$1"
  local install_uri="$2"

  # Try to move without sudo
  if mv "$tmp_file" "$install_uri" 2>/dev/null; then
    :
  else
    run_with_sudo_prompt "mv '$tmp_file' '$install_uri'" "Cannot write to '$install_uri' with current user permissions."
  fi

  # Try to chmod without sudo
  if chmod 755 "$install_uri" 2>/dev/null; then
    :
  else
    run_with_sudo_prompt "chmod 755 '$install_uri'" "Cannot update executable permissions for '$install_uri' with current user permissions."
  fi
}

# Prefer ~/.local/bin if it exists in $PATH, creating it if needed
resolve_install_uri() {
  local fallback="$1"
  local local_bin="${HOME}/.local/bin"

  if echo "$PATH" | tr ':' '\n' | grep -qx "$local_bin"; then
    mkdir -p "$local_bin"
    echo "$local_bin/ilagent"
  else
    echo "$fallback"
  fi
}

TEMP_DIR=$(mktemp -d)
TEMP_FILE="${TEMP_DIR}/ilagent"
trap 'rmdir "$TEMP_DIR" 2>/dev/null || true' EXIT

if [ "$(uname)" == "Darwin" ]; then

  INSTALL_URI=$(resolve_install_uri "/usr/local/bin/ilagent")
  FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_mac"
  echo "[MacOS] Downloading binary.. please be patient."
  if ! curl_download_file "$FILE_URL" "$TEMP_FILE"; then
    echo "Download failed or timed out. Please check your network connection and try again."
    exit 1
  fi
  install_binary "$TEMP_FILE" "$INSTALL_URI"
  echo "Done"
  ilagent --help

elif [ "$(expr substr $(uname -s) 1 5)" == "Linux" ]; then

  if [ "$(expr substr $(uname -m) 1 3)" == "arm" ]; then
    INSTALL_URI=$(resolve_install_uri "/usr/bin/ilagent")
    FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_arm"
    echo "[ARM] Downloading binary.. please be patient."
    if ! curl_download_file "$FILE_URL" "$TEMP_FILE"; then
      echo "Download failed or timed out. Please check your network connection and try again."
      exit 1
    fi
    install_binary "$TEMP_FILE" "$INSTALL_URI"
    echo "Done"
    ilagent --help
  else
    INSTALL_URI=$(resolve_install_uri "/usr/bin/ilagent")
    FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_linux"
    echo "[Linux] Downloading binary.. please be patient."
    if ! curl_download_file "$FILE_URL" "$TEMP_FILE"; then
      echo "Download failed or timed out. Please check your network connection and try again."
      exit 1
    fi
    install_binary "$TEMP_FILE" "$INSTALL_URI"
    echo "Done"
    ilagent --help
  fi

else
  echo "Unsupported platform, please install manually."
fi
