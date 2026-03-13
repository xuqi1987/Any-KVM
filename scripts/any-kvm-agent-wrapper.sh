#!/bin/bash
# Wrapper script: starts Xvfb (for screen capture fallback) then runs the agent.
# Xvfb is only needed when no physical display is available.

export DISPLAY=:99

# Start Xvfb in background if not already running
if ! pgrep -f "Xvfb :99" > /dev/null 2>&1; then
    /usr/bin/Xvfb :99 -screen 0 1280x720x24 -nolisten tcp &
    sleep 1
fi

exec /usr/bin/any-kvm-agent /etc/any-kvm-agent/config.toml
