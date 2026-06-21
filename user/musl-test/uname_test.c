#include <sys/utsname.h>
#include <unistd.h>
#include <string.h>

static void puts_fd(int fd, const char *s) {
    write(fd, s, strlen(s));
    write(fd, "\n", 1);
}

int main(void) {
    struct utsname u;
    if (uname(&u) != 0) {
        puts_fd(2, "uname failed");
        return 1;
    }
    puts_fd(1, "=== uname ===");
    puts_fd(1, u.sysname);
    puts_fd(1, u.nodename);
    puts_fd(1, u.release);
    puts_fd(1, u.machine);
    return 0;
}
