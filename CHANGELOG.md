# ilagent CHANGELOG

## 2021-11-01, Version 0.3.0

* **BREAKING** --incident_key is now --alert_key (-i is still available)
* moved to ilert-rust@2.0.0, will migrated incident_key -> alert_key in code and db
* added event mapping keys to map mqtt payloads to event api
* added event filter keys to filter mqtt payloads

## 2020-08-21, Version 0.2.2

* keep mqtt connection settings on reconnect

## 2020-08-06, Version 0.2.1

* recovery loop for mqtt connection

## 2020-07-14, Version 0.2.0

* starting the changelog