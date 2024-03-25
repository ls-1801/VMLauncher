#!/bin/bash
echo "1234" | su lukas -l -s /bin/bash -c "echo '1234' | sudo -S target/debug/TestCaseLauncher -k script -n 10.0.0.0/24 $(pwd)/input.yaml"
