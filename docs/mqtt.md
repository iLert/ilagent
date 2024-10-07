### Testing MQTT locally

Run the broker:

```sh
docker run -d -p 1883:1883 -p 9001:9001 -v ./mosquitto.conf:/mosquitto/config/mosquitto.conf eclipse-mosquitto
```
Recommended UI: https://mqttx.app