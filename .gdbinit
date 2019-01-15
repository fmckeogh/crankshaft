define hook-quit
    set confirm off
end

target extended-remote :3333

# print demangled symbols
set print asm-demangle on

# detect unhandled exceptions, hard faults and panics
#break DefaultHandler
#break rust_begin_unwind

# *try* to stop at the user entry point (it might be gone due to inlining)
#break main

monitor arm semihosting enable

load

continue