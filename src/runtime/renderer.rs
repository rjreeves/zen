use crate::runtime::values::Value;
use std::collections::HashMap;

pub fn render(value: &Value) {
    match value {
        Value::Null => {}

        Value::Bool(b) => println!("{}", b),

        Value::Number(n) => println!("{}", n),

        Value::String(s) => println!("{}", s),

        Value::List(items) => {
            if items.is_empty() {
                println!("(empty)");
                return;
            }

            match &items[0] {
                Value::Object(_) => render_table(items),
                _ => {
                    for item in items {
                        println!("{:?}", item);
                    }
                }
            }
        }

        Value::Object(obj) => {
            for (k, v) in obj {
                println!("{}: {:?}", k, v);
            }
        }
    }
}

fn render_table(items: &Vec<Value>) {
    let mut headers = Vec::new();

    if let Value::Object(map) = &items[0] {
        headers = map.keys().cloned().collect();
    }

    // Column widths
    let mut widths: HashMap<String, usize> = HashMap::new();

    for h in &headers {
        widths.insert(h.clone(), h.len());
    }

    for item in items {
        if let Value::Object(map) = item {
            for h in &headers {
                if let Some(val) = map.get(h) {
                    let len = format_value(val).len();
                    let entry = widths.get_mut(h).unwrap();
                    if len > *entry {
                        *entry = len;
                    }
                }
            }
        }
    }

    // Print header
    for h in &headers {
        print!("{:<width$}  ", h, width = widths[h]);
    }
    println!();

    // Print separator
    for h in &headers {
        print!("{:-<width$}  ", "", width = widths[h]);
    }
    println!();

    // Print rows
    for item in items {
        if let Value::Object(map) = item {
            for h in &headers {
                let val = map.get(h).map(format_value).unwrap_or_default();
                print!("{:<width$}  ", val, width = widths[h]);
            }
            println!();
        }
    }
}

fn format_value(val: &Value) -> String {
    match val {
        Value::String(s) => s.clone(),
        Value::Number(n) => format!("{}", n),
        Value::Bool(b) => format!("{}", b),
        _ => format!("{:?}", val),
    }
}