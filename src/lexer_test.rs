mod lexer;

use lexer::Lexer;

fn main() {
    let src = r#"
requires {
  fs.read
}

files = fs.list "C:\logs"

files | where size > 1mb | select name, size
"#;

    let tokens = Lexer::new(src).tokenize().unwrap();
    for t in tokens {
        println!("{:?}", t);
    }
}