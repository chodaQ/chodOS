#include <unistd.h>
#include <string.h>

int main(int argc, char *argv[]) {
    const char *msg = "Hello from musl-static!\n";
    write(1, msg, strlen(msg));

    /* argc/argv 확인 */
    char buf[64];
    buf[0] = 'a'; buf[1] = 'r'; buf[2] = 'g'; buf[3] = 'c'; buf[4] = '=';
    buf[5] = '0' + (argc % 10);
    buf[6] = '\n';
    write(1, buf, 7);

    return 0;
}
