/*
 * proton-presence-bridge: lets a Windows game running under Wine/Proton
 * reach Discord on the Linux side.
 *
 * Games with their own Discord Rich Presence talk to Discord through the
 * Windows named pipe \\.\pipe\discord-ipc-0. Inside a Wine prefix nothing
 * provides that pipe, so the game's presence is lost. This helper creates
 * the pipe and relays every connection to the proton-presence daemon over
 * TCP on 127.0.0.1; the daemon passes the bytes on to Discord's socket.
 *
 * It is started by the daemon, never by hand. The daemon's port and a
 * secret token arrive in the environment variable
 *
 *     PROTON_PRESENCE_BRIDGE=<port>:<token>
 *
 * A control connection to the daemon is kept open for the helper's whole
 * life. When it closes (the game ended or the daemon stopped) the helper
 * exits, so it can never keep a Wine prefix alive on its own.
 *
 * Build (this is what build.sh runs):
 *   x86_64-w64-mingw32-gcc -Os -s -Wall -Wextra -Wl,--no-insert-timestamp \
 *       -o proton-presence-bridge.exe bridge.c -lws2_32
 */

#include <winsock2.h>
#include <windows.h>
#include <stdlib.h>
#include <string.h>

#define PIPE_NAME "\\\\.\\pipe\\discord-ipc-0"
#define BUF_SIZE 65536

static unsigned short daemon_port;
static char daemon_token[128];

/* Connects to the daemon and announces what the connection is for:
 * 'C' for the control connection, 'D' for a relayed game connection. */
static SOCKET connect_daemon(char kind)
{
    struct sockaddr_in addr;
    char hello[160];
    int len;
    SOCKET sock = socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);

    if (sock == INVALID_SOCKET)
        return INVALID_SOCKET;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_port = htons(daemon_port);
    addr.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
    if (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
        closesocket(sock);
        return INVALID_SOCKET;
    }
    hello[0] = kind;
    hello[1] = ' ';
    len = (int)strlen(daemon_token);
    memcpy(hello + 2, daemon_token, len);
    hello[2 + len] = '\n';
    if (send(sock, hello, len + 3, 0) != len + 3) {
        closesocket(sock);
        return INVALID_SOCKET;
    }
    return sock;
}

/* Overlapped read or write that waits for completion. The pipe is opened
 * for overlapped I/O so that reading and writing can happen at the same
 * time from two threads. */
static BOOL pipe_io(HANDLE pipe, BOOL write, char *buf, DWORD len, DWORD *done)
{
    OVERLAPPED ov;
    BOOL ok;

    memset(&ov, 0, sizeof(ov));
    ov.hEvent = CreateEventA(NULL, TRUE, FALSE, NULL);
    if (!ov.hEvent)
        return FALSE;
    ok = write ? WriteFile(pipe, buf, len, NULL, &ov)
               : ReadFile(pipe, buf, len, NULL, &ov);
    if (!ok && GetLastError() != ERROR_IO_PENDING) {
        CloseHandle(ov.hEvent);
        return FALSE;
    }
    ok = GetOverlappedResult(pipe, &ov, done, TRUE);
    CloseHandle(ov.hEvent);
    return ok;
}

struct relay {
    HANDLE pipe;
    SOCKET sock;
};

/* Game -> daemon. */
static DWORD WINAPI pipe_to_socket(LPVOID arg)
{
    struct relay *relay = arg;
    char *buf = malloc(BUF_SIZE);
    DWORD got;

    while (buf && pipe_io(relay->pipe, FALSE, buf, BUF_SIZE, &got) && got > 0) {
        DWORD sent = 0;
        while (sent < got) {
            int n = send(relay->sock, buf + sent, (int)(got - sent), 0);
            if (n <= 0)
                goto out;
            sent += (DWORD)n;
        }
    }
out:
    /* Wakes the other direction, which is blocked in recv(). */
    shutdown(relay->sock, SD_BOTH);
    free(buf);
    return 0;
}

/* One game connection: daemon -> game here, game -> daemon in a thread. */
static DWORD WINAPI serve(LPVOID arg)
{
    struct relay relay;
    HANDLE reader;
    char *buf = malloc(BUF_SIZE);

    relay.pipe = (HANDLE)arg;
    relay.sock = connect_daemon('D');
    if (buf && relay.sock != INVALID_SOCKET) {
        reader = CreateThread(NULL, 0, pipe_to_socket, &relay, 0, NULL);
        if (reader) {
            for (;;) {
                DWORD written = 0, done;
                int got = recv(relay.sock, buf, BUF_SIZE, 0);
                if (got <= 0)
                    break;
                while (written < (DWORD)got) {
                    if (!pipe_io(relay.pipe, TRUE, buf + written,
                                 (DWORD)got - written, &done) || done == 0)
                        goto stop;
                    written += done;
                }
            }
stop:
            /* Wakes the reader thread, which is blocked on the pipe. */
            CancelIoEx(relay.pipe, NULL);
            shutdown(relay.sock, SD_BOTH);
            WaitForSingleObject(reader, INFINITE);
            CloseHandle(reader);
        }
    }
    if (relay.sock != INVALID_SOCKET)
        closesocket(relay.sock);
    FlushFileBuffers(relay.pipe);
    DisconnectNamedPipe(relay.pipe);
    CloseHandle(relay.pipe);
    free(buf);
    return 0;
}

/* Exits the process as soon as the daemon drops the control connection. */
static DWORD WINAPI watch_control(LPVOID arg)
{
    SOCKET control = (SOCKET)(UINT_PTR)arg;
    char byte;

    while (recv(control, &byte, 1, 0) > 0)
        ;
    ExitProcess(0);
    return 0;
}

int main(void)
{
    WSADATA wsa;
    SOCKET control;
    const char *config = getenv("PROTON_PRESENCE_BRIDGE");
    const char *colon = config ? strchr(config, ':') : NULL;
    BOOL first = TRUE;

    if (!colon || strlen(colon + 1) >= sizeof(daemon_token))
        return 2;
    daemon_port = (unsigned short)atoi(config);
    strcpy(daemon_token, colon + 1);
    if (daemon_port == 0 || daemon_token[0] == '\0')
        return 2;
    if (WSAStartup(MAKEWORD(2, 2), &wsa) != 0)
        return 3;

    control = connect_daemon('C');
    if (control == INVALID_SOCKET)
        return 4;
    if (!CreateThread(NULL, 0, watch_control, (LPVOID)(UINT_PTR)control, 0, NULL))
        return 5;

    for (;;) {
        OVERLAPPED ov;
        DWORD ignored;
        HANDLE thread;
        /* If the pipe already exists, another bridge is serving this
         * prefix. Leave it alone rather than compete with it. */
        DWORD open_mode = PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED |
                          (first ? FILE_FLAG_FIRST_PIPE_INSTANCE : 0);
        HANDLE pipe = CreateNamedPipeA(PIPE_NAME, open_mode,
                                       PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                                       PIPE_UNLIMITED_INSTANCES, BUF_SIZE, BUF_SIZE,
                                       0, NULL);
        if (pipe == INVALID_HANDLE_VALUE)
            return first ? 6 : 7;
        first = FALSE;

        memset(&ov, 0, sizeof(ov));
        ov.hEvent = CreateEventA(NULL, TRUE, FALSE, NULL);
        if (!ov.hEvent)
            return 8;
        if (!ConnectNamedPipe(pipe, &ov)) {
            DWORD err = GetLastError();
            if (err == ERROR_IO_PENDING) {
                if (!GetOverlappedResult(pipe, &ov, &ignored, TRUE))
                    err = GetLastError();
                else
                    err = ERROR_PIPE_CONNECTED;
            }
            if (err != ERROR_PIPE_CONNECTED) {
                CloseHandle(ov.hEvent);
                CloseHandle(pipe);
                Sleep(100);
                continue;
            }
        }
        CloseHandle(ov.hEvent);

        thread = CreateThread(NULL, 0, serve, pipe, 0, NULL);
        if (thread)
            CloseHandle(thread);
        else
            CloseHandle(pipe);
    }
}
