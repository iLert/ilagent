#!/usr/bin/env bash
set -e
if [ -z "${DEBUG}" ]; then
  set +o xtrace
else
  set -o xtrace
fi

VERSION="0.7.0"

# Prompt user to run a command with sudo; show exact command first
run_with_sudo_prompt() {
  local cmd="$1"
  echo ""
  echo "Insufficient permissions. The following command needs to be run with sudo:"
  echo "  sudo $cmd"
  read -r -p "Run with sudo? [y/N] " answer
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

  # Remove existing binary if present
  rm -f "$install_uri" 2>/dev/null || sudo rm -f "$install_uri" 2>/dev/null || true

  # Try to move without sudo
  if mv "$tmp_file" "$install_uri" 2>/dev/null; then
    :
  else
    run_with_sudo_prompt "mv '$tmp_file' '$install_uri'"
  fi

  # Try to chmod without sudo
  if chmod 755 "$install_uri" 2>/dev/null; then
    :
  else
    run_with_sudo_prompt "chmod 755 '$install_uri'"
  fi
}

TEMP_DIR=$(mktemp -d)
TEMP_FILE="${TEMP_DIR}/ilagent"
trap 'rm -rf "$TEMP_DIR"' EXIT

if [ "$(uname)" == "Darwin" ]; then

  INSTALL_URI="/usr/local/bin/ilagent"
  FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_mac"
  echo "[MacOS] Downloading binary.. please be patient."
  curl -sLS "$FILE_URL" --output "$TEMP_FILE"
  install_binary "$TEMP_FILE" "$INSTALL_URI"
  echo "Done"
  ilagent --help

elif [ "$(expr substr $(uname -s) 1 5)" == "Linux" ]; then

  if [ "$(expr substr $(uname -m) 1 3)" == "arm" ]; then
    INSTALL_URI="/usr/bin/ilagent"
    FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_arm"
    echo "[ARM] Downloading binary.. please be patient."
    curl -sLS "$FILE_URL" --output "$TEMP_FILE"
    install_binary "$TEMP_FILE" "$INSTALL_URI"
    echo "Done"
    ilagent --help
  else
    INSTALL_URI="/usr/bin/ilagent"
    FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_linux"
    echo "[Linux] Downloading binary.. please be patient."
    curl -sLS "$FILE_URL" --output "$TEMP_FILE"
    install_binary "$TEMP_FILE" "$INSTALL_URI"
    echo "Done"
    ilagent --help
  fi

else
  echo "Unsupported platform, please install manually."
fi
