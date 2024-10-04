# ilagent CHANGELOG

## 2024-10-04, Version 0.5.0

* upgraded dependencies
* migrated from sync threads to a tokio app
* **BREAKING** --mqtt_* prefixed event mapping arguments have dropped the prefix to fit to other consumers as well
* now supporting Apache Kafka to event API proxy
* bumped the docker image to rust 1.81
* bumped SQLite version from 3.41.2 -> 3.46.0

## 2023-05-13, Version 0.4.0

* upgraded dependencies
* bumped SQLite from 3.36.0 to 3.41.2
* added new `cleanup` command
* added cleanup command resource `alerts`

## 2021-11-02, Version 0.3.0

* **BREAKING** --incident_key is now --alert_key (-i is still available)
* **BREAKING** http server is not started unless --p is provided
* **BREAKING** migrated to new API /api/v1/events -> /api/events
* if one of the threads exit, the whole program will exit
* moved to ilert-rust@2.0.0, will migrated incident_key -> alert_key in code and db
* added event mapping keys to map mqtt payloads to event api
* added event filter keys to filter mqtt payloads

## 2020-08-21, Version 0.2.2

* keep mqtt connection settings on reconnect

## 2020-08-06, Version 0.2.1

* recovery loop for mqtt connection

## 2020-07-14, Version 0.2.0

* starting the changelog