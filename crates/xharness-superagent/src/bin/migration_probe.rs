use rusqlite::Connection;
use xharness_superagent::store::Store;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 {
        return Err("expected disposable schema-10 path and cut name".into());
    }
    let store = Store::raw(Connection::open(&args[1])?, Box::new(|_| {}));
    let cut = args[2].clone();
    store.initialize_with_hooks(&|| 1793404800000, &mut |point| {
        if cut == point {
            std::process::exit(if point == "after-commit" { 89 } else { 88 });
        }
        Ok(())
    })?;
    Err("unknown crash cut point".into())
}
