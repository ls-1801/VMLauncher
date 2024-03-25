QUERY=$1
# (src 10.0.0.1 and src port 8091) or (src 10.0.0.2 and dst port 8432)
echo "1234" | su lukas -l -s /bin/bash -c "echo '1234' | sudo -S tcpdump -i tbr0 \"${QUERY}\" > $(pwd)/tcp_dump.txt"
