use std::thread;
use log::info;
use std::time::Duration;

mod server;
use server::run_server;

mod queue;
use queue::Queue;

fn main() -> () {
    env_logger::init();

    let poll_thread = thread::spawn(move || {
        let mut round = 0;
        loop {
            round = round + 1;
            if round > 3 {
                break;
            }
            info!("Test: {}", round);
            thread::sleep(Duration::new(3, 0));
        }
    });

    let db_test = thread::spawn(move || {
        let queue = Queue::new("./ilagent.db3");
        queue.prepare_database();
    });

    info!("Starting server..");
    let http_bind_str = format!("0.0.0.0:{}", 8977);
    run_server(http_bind_str).unwrap();

    poll_thread.join().unwrap();
    db_test.join().unwrap();

    ()
}