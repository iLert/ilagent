1. ~~create ilert (api) rust crate / map only eventing api for now~~
2. use ilert rust crate to setup ilclient.rs, use client to call daemon api
3. finish rest api for ilserver
4. make event api calls for poll
5. add direct mode for create event to skip sqlite storage
6. add heartbeat to db to test migration
7. add heartbeat to additional polljob
8. implement heartbeat cli
9. add cross compile for releases cross compile via "https://github.com/rust-embedded/cross" 
10. add markdown documentation
11. make repos public and build first release