#!/usr/bin/env bash

INSTALL_URI="/usr/local/bin/ilagent"

if [ "$(uname)" == "Darwin" ]; then

  FILE_URL="https://github.com/iLert/ilagent/releases/download/0.2.0/ilagent_mac"
  rm $INSTALL_URI || true
  curl -sL $FILE_URL --output $INSTALL_URI
  chmod 777 $INSTALL_URI
  ilagent -V

elif [ "$(expr substr $(uname -s) 1 5)" == "Linux" ]; then

  FILE_URL="https://github.com/iLert/ilagent/releases/download/0.2.0/ilagent_linux"
  rm $INSTALL_URI || true
  curl -sL $FILE_URL --output $INSTALL_URI
  chmod 777 $INSTALL_URI
  ilagent -V

elif [ "$(expr substr $(uname -s) 1 10)" == "MINGW32_NT" ]; then
    echo "Unsupported platform, please install manually."
elif [ "$(expr substr $(uname -s) 1 10)" == "MINGW64_NT" ]; then
    echo "Unsupported platform, please install manually."
fi
