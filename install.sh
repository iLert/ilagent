#!/usr/bin/env bash
set -e
if [ -z "${DEBUG}" ]; then
  set +o xtrace
else
  set -o xtrace
fi

VERSION="0.7.0"

check_writable() {
  local dir
  dir=$(dirname "$1")
  if [ ! -w "$dir" ]; then
    echo "Error: No write permission to $dir. Please re-run with: sudo bash install.sh"
    exit 1
  fi
}

if [ "$(uname)" == "Darwin" ]; then

  INSTALL_URI="/usr/local/bin/ilagent"
  check_writable "$INSTALL_URI"
  FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_mac"
  rm $INSTALL_URI || true
  echo "[MacOS] Downloading binary.. please be patient."
  curl -sLS $FILE_URL --output $INSTALL_URI
  chmod 777 $INSTALL_URI
  echo "Done"
  ilagent --help

elif [ "$(expr substr $(uname -s) 1 5)" == "Linux" ]; then

  if [ "$(expr substr $(uname -m) 1 3)" == "arm" ]; then
    INSTALL_URI="/usr/bin/ilagent"
    check_writable "$INSTALL_URI"
    FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_arm"
    rm $INSTALL_URI || true
    echo "[ARM] Downloading binary.. please be patient."
    curl -sLS $FILE_URL --output $INSTALL_URI
    chmod 777 $INSTALL_URI
    echo "Done"
    ilagent --help
  else
    INSTALL_URI="/usr/bin/ilagent"
    check_writable "$INSTALL_URI"
    FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_linux"
    rm $INSTALL_URI || true
    echo "[Linux] Downloading binary.. please be patient."
    curl -sLS $FILE_URL --output $INSTALL_URI
    chmod 777 $INSTALL_URI
    echo "Done"
    ilagent --help
  fi

else
  echo "Unsupported platform, please install manually."
fi
