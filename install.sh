#!/usr/bin/env bash
set -e
if [ -z "${DEBUG}" ]; then
  set +o xtrace
else
  set -o xtrace
fi

VERSION="0.2.0"
INSTALL_URI="/usr/local/bin/ilagent"

if [ "$(uname)" == "Darwin" ]; then

  FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_mac"
  rm $INSTALL_URI || true
  echo "[MacOS] Downloading binary.. please be patient."
  curl -sL $FILE_URL --output $INSTALL_URI
  chmod 777 $INSTALL_URI
  echo "Done"
  ilagent --help

elif [ "$(expr substr $(uname -s) 1 5)" == "Linux" ]; then

  if [ "$(expr substr $(uname -m) 1 3)" == "arm" ]; then
    FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_arm"
    rm $INSTALL_URI || true
    echo "[ARM] Downloading binary.. please be patient."
    curl -sL $FILE_URL --output $INSTALL_URI
    chmod 777 $INSTALL_URI
    echo "Done"
    ilagent --help
  else
    FILE_URL="https://github.com/iLert/ilagent/releases/download/${VERSION}/ilagent_linux"
    rm $INSTALL_URI || true
    echo "[Linux] Downloading binary.. please be patient."
    curl -sL $FILE_URL --output $INSTALL_URI
    chmod 777 $INSTALL_URI
    echo "Done"
    ilagent --help
  fi

else
  echo "Unsupported platform, please install manually."
fi
