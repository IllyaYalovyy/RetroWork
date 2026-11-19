use std::{thread, time::Duration};

fn main() {
    for tick in 1..=1200 {
        println!("tick {tick}");
        thread::sleep(Duration::from_millis(100));
    }
}
