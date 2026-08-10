# 15. Current language sketch

The following example combines the currently agreed ideas. File APIs remain
illustrative and are not part of the initial built-in surface:

```text
interface Reader {
    fn read(mut self, count: int) -> bytes;
}

interface Writer {
    fn write(mut self, data: bytes) -> int;
}

struct Buffer {
    data: bytes,
    position: int,

    fn read(mut self, count: int) -> bytes {
        // Implementation omitted.
    }

    fn write(mut self, data: bytes) -> int {
        // Implementation omitted.
    }
}

fn find_nonzero(data: bytes) -> int | none {
    for index in 0..data.length() {
        const value = data[index];

        if value != 0 {
            break value;
        }
    } else {
        none
    }
}

fn copy_once(mut stream: Reader & Writer) -> int {
    const data = stream.read(4096);
    stream.write(data)
}

fn prefixed_writer(prefix: bytes, mut destination: Writer) -> mut Writer {
    Writer {
        fn write(mut self, data: bytes) -> int {
            destination.write(bytes.concat(prefix, data))
        }
    }
}

fn make_prefixer(prefix: bytes) -> fn(bytes) -> bytes {
    lambda(data: bytes) -> bytes {
        bytes.concat(prefix, data)
    }
}

fn write_file(path: string, data: bytes) -> int {
    mut file = File.create(path);
    defer file.close();

    file.write(data)
}
```

The anonymous writer and lambda examples use automatic lexical capture. Their
environments use the specialized garbage-collected layouts described above.
The file example shows the intended lexical cleanup behaviour without allowing
a resource to escape its scope.
