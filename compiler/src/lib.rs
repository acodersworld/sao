pub mod ast;
pub mod context_resolution;
mod expression_analysis;
pub mod lexer;
pub mod name_resolution;
pub mod parser;
pub mod pretty;
pub mod semantic_types;
pub mod signature_collection;
pub mod source;
pub mod symbol_table;
pub mod type_resolution;

#[cfg(test)]
mod tests {
    use crate::{
        context_resolution::resolve_program_context,
        expression_analysis::assert_program_checks,
        lexer::Lexer,
        name_resolution::resolve_program,
        parser::{ParseContext, parse_program},
        signature_collection::collect_signatures,
        source::SourceModuleRegistry,
        type_resolution::resolve_types,
    };

    static COMPLEX_PROGRAM: &str = r#"
interface Formatter {
    fn format(self, prefix: string) -> string;
}

interface Accumulator {
    fn add(mut self, amount: int) -> int;
}

type SummaryAlias = Summary;

fn Box(comptime T: type) -> type {
    struct {
        inner: T,

        fn get(self) -> T {
            self.inner
        }

        fn echo(self, comptime U: type, value: U) -> U {
            value
        }
    }
}

type IntBox = Box(int);

type Metric = (int, string);

fn Pair(comptime T: type) -> type {
    (T, T)
}

type IntPair = Pair(int);

fn tuple_identity(comptime T: type, value: T) -> T {
    value
}

fn tuple_total(value: Metric) -> int {
    value.0
}

fn inspect_inline(comptime T: Formatter, value: T) -> string {
    value.format("inline: ")
}

fn inspect_named(comptime T: type, value: T) -> string
where T: Formatter, {
    value.format("named: ")
}

fn inspect_private(comptime T: type, value: T) -> string
where T: interface { fn format(self, prefix: string) -> string; }, {
    value.format("private: ")
}

struct Summary {
    name: string,
    total: int,
    next: &Summary | none,

    fn new(name: string) -> Summary {
        Summary { name: name + "", total: 0, next: none }
    }

    fn add(mut self, amount: int) -> int {
        self.total += amount;
        self.total
    }

    fn format(&self, prefix: string) -> string {
        prefix + self.name
    }

    fn format_with(self, comptime T: Formatter, formatter: T) -> string {
        formatter.format(self.name)
    }
}

struct BriefFormatter {
    fn format(self, prefix: string) -> string {
        prefix + "brief"
    }
}

struct DetailedFormatter {
    fn format(self, prefix: string) -> string {
        prefix + "detailed"
    }
}

fn cleanup(summary: Summary) {
    print(summary.name);
}

fn worker(value: int) {
    println("working");
    yield();
}

fn checked(value: int) -> Error(int) {
    Error::new(value)
}

fn apply(value: int, operation: fn(int) -> int) -> int {
    operation(value)
}

fn exercise_sequences(const vmut left: bytes, right: bytes) {
    left[0] = 255;
    left[1] += 1;
    const joined = bytes::concat(left, right);
    const copied = joined[0..joined.length()];
    copied[0];
}

fn main() {
    const initial = 3;
    mut summary: Summary = Summary::new("language tour");
    const vmut shared_summary = summary;
    const heap_summary = &Summary::new("shared");
    mut vconst stable_seed = initial;

    fn fibonacci(value: int) -> int {
        if value <= 1 {
            return value;
        }
        fibonacci(value - 1) + fibonacci(value - 2)
    }

    fn announce(value: int) {
        println("value");
    }

    fn announce_if_integer(value: int | float | none) {
        if !(value is int) {
            return;
        }
        announce(value);
    }

    const scale: fn(int) -> int = lambda(value: int) -> int {
        value * initial
    };
    const heap_scale: &fn(int) -> int = &lambda(value: int) -> int {
        return scale(value + 1);
    };
    const notify = lambda {
        announce(initial);
    };

    const vmut formatter_implementation = struct {
        prefix: string = "total: ";
        offset = initial;

        fn format(self, suffix: string) -> string {
            self.prefix + suffix
        }

        fn add(mut self, amount: int) -> int {
            self.offset + amount
        }
    };
    const formatter: Formatter = formatter_implementation;
    const heap_formatter: &Formatter = &struct {
        fn format(self, value: string) -> string {
            value
        }
    };
    const vmut capabilities: Formatter & Accumulator = formatter_implementation;
    const formatter_only: Formatter = capabilities;
    const selected_capability: Formatter | Accumulator = formatter_implementation: Formatter;
    const formatter_choice: BriefFormatter | DetailedFormatter = if true {
        BriefFormatter {}
    } else {
        DetailedFormatter {}
    };
    const formatter_view: Formatter = formatter_choice;
    const optional: int | none = none;

    const queue: Queue(int) = Queue(int)::new();
    const vector: Vector(int) = Vector(int)::new();
    const table: Map(string, int) = Map(string, int)::new();
    const failure: Error(int) = checked(initial);
    const recovered = failure?;

    const integer = 1.5: int;
    const decimal = integer: float;
    const truth: bool = decimal != 0.0;
    const numeric: int | float = if truth {
        integer
    } else {
        decimal
    };
    const wider_numeric: int | float | none = numeric;
    const grouped_initial = { initial };
    const character = 65: char;
    const character_code = character: int;
    const text = "A";
    const middle = text[0..1];
    const text_length = text.length();
    const literal_middle = "sequence"[1..4];
    const literal_length = "sequence".length();
    const first_character = "sequence"[0];
    const indexed = vector[0];
    const calculated = apply(fibonacci(initial), scale);
    const flags = (calculated << 1) | (initial & 1) ^ 2;
    const comparison = calculated >= initial && truth || false;
    const formatted = f"{{total}} {summary.name:<20}: {calculated:+08} ({decimal:.2f})";
    const boxed: IntBox = Box(int) { inner: calculated };
    const inline_inspection = inspect_inline(BriefFormatter, BriefFormatter {});
    const named_inspection = inspect_named(DetailedFormatter, DetailedFormatter {});
    const private_inspection = inspect_private(BriefFormatter, BriefFormatter {});
    const method_inspection = summary.format_with(BriefFormatter, BriefFormatter {});
    const boxed_echo = boxed.echo(string, "boxed");
    const boxed_tuple = boxed.echo((int, string), (calculated, "method"));
    mut metric: Metric = (initial, "tuple");
    metric.0 += 1;
    const metric_copy = metric.copy();
    const tuple_specialization = tuple_identity((int, string), (calculated, "specialized"));
    const tuple_factory: IntPair = (initial, calculated);
    const tuple_choice: Metric | (string, int) = (calculated, "choice");
    const tuple_vector: Vector(Metric) = Vector(Metric)::new();
    const heap_tuple = &(calculated, Summary::new("tuple heap"));

    summary.total = calculated;
    summary.total += recovered;
    vector[0] = flags;
    stable_seed += 1;

    defer cleanup(summary);
    co worker(initial);
    announce(grouped_initial);
    announce(indexed);
    announce_if_integer(wider_numeric);
    formatter.format(middle);
    formatter.format(literal_middle);
    announce(text_length);
    announce(literal_length);
    first_character;
    character;
    announce(character_code);
    println(formatted);
    println(inline_inspection);
    println(named_inspection);
    println(private_inspection);
    println(method_inspection);
    println(boxed_echo);
    announce(boxed_tuple.0);
    announce(boxed.get());
    announce(tuple_total(metric_copy));
    announce(tuple_specialization.0);
    announce(tuple_factory.1);
    if tuple_choice is Metric {
        announce(tuple_choice.0);
    };
    tuple_vector.length();
    announce(heap_tuple.0);
    heap_formatter.format(text);
    heap_scale(initial);
    notify();
    heap_summary.format("shared: ");
    capabilities.format("capabilities: ");
    formatter_only.format("reduced: ");
    formatter_view.format("union: ");
    wider_numeric;
    queue.length();
    table.length();

    if comparison && numeric is int {
        announce(numeric);
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

    summary;
}
"#;

    #[test]
    fn compiles_a_complex_program_through_the_available_frontend() {
        let module = SourceModuleRegistry::new().add(COMPLEX_PROGRAM);
        let mut parse_context = ParseContext::new(module.module_id());
        let program = parse_program(&mut parse_context, Lexer::new(&module))
            .expect("the complex program should lex and parse");

        let names = resolve_program(&module, &program)
            .expect("every value and type name in the complex program should resolve");
        let context = resolve_program_context(&program)
            .expect("every callable and control-flow context should be valid");
        let mut types = resolve_types(&module, &program, &names)
            .expect("every source type in the complex program should resolve");
        let signatures = collect_signatures(&module, &program, &names, &context, &mut types)
            .expect("every declaration signature in the complex program should collect");

        assert_program_checks(&module, &program, &names, &context, &signatures, &mut types);
    }
}
