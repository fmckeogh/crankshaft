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

monitor tpiu config internal itm.fifo uart off 100000000

monitor itm port 0 on

echo clear EXCEVTENA; set PCSAMPLENA\n
monitor mmw 0xE0001000 4096 65536
echo enable CYCCNT; set POSTINIT / POSTRESET to 3\n
monitor mmw 0xE0001000 103 510

load

continue