pub mod ast;
pub mod context_resolution;
pub mod lexer;
pub mod name_resolution;
pub mod parser;
pub mod pretty;
pub mod semantic_types;
pub mod source;
pub mod symbol_table;

#[cfg(test)]
mod tests {
    use crate::{
        context_resolution::resolve_program_context,
        lexer::Lexer,
        name_resolution::resolve_program,
        parser::{ParseContext, parse_program},
        source::SourceModuleRegistry,
    };

    static COMPLEX_PROGRAM: &str = r#"
interface Formatter {
    fn format(&self, prefix: string) -> string;
}

interface Accumulator {
    fn add(mut self, amount: int) -> int;
}

struct Summary {
    name: string,
    total: int,
    next: Summary | none,

    fn new(name: string) -> Summary {
        Summary { name: name, total: 0, next: none }
    }

    fn add(mut self, amount: int) -> int {
        self.total += amount;
        self.total
    }

    fn format(&self, prefix: string) -> string {
        prefix + self.name + string(self.total)
    }
}

fn cleanup(summary: Summary) {
    print(summary.name);
}

fn worker(value: int) {
    println(string(value));
    yield();
}

fn checked(value: int) -> Error<int> {
    Error::new(value)
}

fn apply(value: int, operation: fn(int) -> int) -> int {
    operation(value)
}

fn main() -> Summary {
    const initial = 3;
    mut summary: Summary = Summary::new("language tour");
    const vmut shared_summary = summary;
    mut vconst stable_seed = initial;

    fn fibonacci(value: int) -> int {
        if value <= 1 {
            return value;
        }
        fibonacci(value - 1) + fibonacci(value - 2)
    }

    fn announce(value: int) {
        println(string(value));
    }

    const scale: fn(int) -> int = lambda(value: int) -> int {
        value * initial
    };
    const heap_scale: &fn(int) -> int = &lambda(value: int) -> int {
        return scale(value + 1);
    };

    const formatter: Formatter = struct {
        prefix: string = "total: ";
        offset = initial;

        fn format(self, suffix: string) -> string {
            self.prefix + suffix + string(self.offset)
        }
    };
    const heap_formatter: &Formatter = &struct {
        fn format(self, value: string) -> string {
            value
        }
    };
    const capabilities: Formatter & Accumulator = formatter;
    const optional: int | none = none;

    const queue: Queue<int> = Queue<int>::new();
    const vector: Vector<int> = Vector<int>::new();
    const table: Map<string, int> = Map<string, int>::new();
    const failure: Error<int> = checked(initial);
    const recovered = failure?;

    const integer = int(1.5);
    const decimal = float(integer);
    const truth: bool = decimal != 0.0;
    const character = char(65);
    const text = string(character);
    const middle = text[0..1];
    const indexed = vector[0];
    const calculated = apply(fibonacci(initial), scale);
    const flags = (calculated << 1) | (initial & 1) ^ 2;
    const comparison = calculated >= initial && truth || false;
    const is_summary = summary is Summary;

    summary.total = calculated;
    summary.total += recovered;
    vector[0] = flags;
    stable_seed += 1;

    defer cleanup(summary);
    co worker(initial);
    announce(indexed);
    formatter.format(middle);
    heap_formatter.format(text);
    heap_scale(initial);
    shared_summary.format("shared: ");
    capabilities.format("capabilities: ");
    queue.length();
    table.length();

    if comparison && is_summary {
        summary.add(initial);
    } else if optional is none {
        summary.add(-1);
    } else {
        summary.add(0);
    };

    mut countdown = 3;
    while countdown > 0 {
        countdown -= 1;
        if countdown == 1 {
            continue;
        }
    } else {
        announce(countdown);
    };

    for index in 0..=3 {
        if index == 2 {
            break;
        }
        summary.add(index);
    } else {
        summary.add(10);
    };

    const selected = loop {
        if summary.total > 0 {
            break summary.total;
        }
        break 0;
    };
    announce(selected);

    {
        const scoped = "done";
        println(scoped);
    };

    summary
}
"#;

    #[test]
    fn compiles_a_complex_program_through_the_available_frontend() {
        let module = SourceModuleRegistry::new().add(COMPLEX_PROGRAM);
        let mut parse_context = ParseContext::new(module.module_id());
        let program = parse_program(&mut parse_context, Lexer::new(&module))
            .expect("the complex program should lex and parse");

        resolve_program(&module, &program)
            .expect("every value and type name in the complex program should resolve");
        resolve_program_context(&program)
            .expect("every callable and control-flow context should be valid");
    }
}
