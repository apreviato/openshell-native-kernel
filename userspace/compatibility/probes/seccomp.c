// Seccomp behavior that the Chromium sandbox depends on. Each case runs in its
// own process and reports one JSON line. The probe exits with zero only if every
// case observed the filter being applied.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/seccomp.h>
#include <pthread.h>
#include <signal.h>
#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <ucontext.h>
#include <unistd.h>

#define ARRAY_SIZE(array) (sizeof(array) / sizeof((array)[0]))
#define LOAD_NR BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr))
#define RETURN(value) BPF_STMT(BPF_RET | BPF_K, (value))
#define ON_SYSCALL(nr, value) BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, (nr), 0, 1), RETURN(value)
#ifndef SYS_SECCOMP
#define SYS_SECCOMP 1
#endif
#define TRAP_DATA 0x2a
#define TRAP_RESULT 777

#define REQUIRE(condition)                                                    \
    do {                                                                      \
        if (!(condition)) {                                                   \
            dprintf(STDERR_FILENO, "seccomp probe: line %d: %s (errno %d)\n", \
                    __LINE__, #condition, errno);                             \
            _exit(1);                                                         \
        }                                                                     \
    } while (0)

static const char *self_path;
// The probe may itself start under a filter, as it does in a container.
static int initial_mode;
static int initial_filters;

static long install(unsigned int flags, struct sock_filter *filter, size_t length) {
    struct sock_fprog program = {.len = (unsigned short)length, .filter = filter};
    return syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, flags, &program);
}

static void no_new_privs(void) {
    REQUIRE(prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0);
}

static void deny_getppid(unsigned int flags) {
    struct sock_filter filter[] = {
        LOAD_NR,
        ON_SYSCALL(SYS_getppid, SECCOMP_RET_ERRNO | EPERM),
        RETURN(SECCOMP_RET_ALLOW),
    };
    REQUIRE(install(flags, filter, ARRAY_SIZE(filter)) == 0);
}

static int getppid_is_denied(void) {
    errno = 0;
    return syscall(SYS_getppid) == -1 && errno == EPERM;
}

static int status_field(const char *name) {
    char line[256];
    int value = -1;
    size_t length = strlen(name);
    FILE *status = fopen("/proc/self/status", "r");
    REQUIRE(status != NULL);
    while (fgets(line, sizeof(line), status) != NULL) {
        if (strncmp(line, name, length) == 0 && line[length] == ':') {
            value = atoi(line + length + 1);
        }
    }
    fclose(status);
    return value;
}

// The kernel reports support by rejecting the null program address.
static void case_support_probe(void) {
    errno = 0;
    REQUIRE(prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, NULL, 0, 0) == -1 && errno == EFAULT);
    errno = 0;
    REQUIRE(syscall(SYS_seccomp, SECCOMP_SET_MODE_FILTER, SECCOMP_FILTER_FLAG_TSYNC, NULL) == -1 &&
            errno == EFAULT);
    REQUIRE(prctl(PR_GET_SECCOMP, 0, 0, 0, 0) == initial_mode);
    unsigned int action = SECCOMP_RET_TRAP;
    REQUIRE(syscall(SYS_seccomp, SECCOMP_GET_ACTION_AVAIL, 0, &action) == 0);
}

static void case_requires_no_new_privs(void) {
    struct sock_filter filter[] = {RETURN(SECCOMP_RET_ALLOW)};
    // An unprivileged process must not filter a program that gains privileges.
    REQUIRE(setresgid(65534, 65534, 65534) == 0 || geteuid() != 0);
    REQUIRE(setresuid(65534, 65534, 65534) == 0 || geteuid() != 0);
    errno = 0;
    REQUIRE(install(0, filter, ARRAY_SIZE(filter)) == -1 && errno == EACCES);
    no_new_privs();
    REQUIRE(install(0, filter, ARRAY_SIZE(filter)) == 0);
}

static void case_rejects_invalid_programs(void) {
    struct sock_filter no_return[] = {LOAD_NR};
    struct sock_filter out_of_bounds_load[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, sizeof(struct seccomp_data)),
        RETURN(SECCOMP_RET_ALLOW),
    };
    struct sock_filter out_of_bounds_jump[] = {
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, 0, 1, 0),
        RETURN(SECCOMP_RET_ALLOW),
    };
    struct sock_filter packet_load[] = {
        BPF_STMT(BPF_LD | BPF_B | BPF_ABS, 0),
        RETURN(SECCOMP_RET_ALLOW),
    };
    struct sock_filter allow[] = {RETURN(SECCOMP_RET_ALLOW)};
    no_new_privs();
    errno = 0;
    REQUIRE(install(0, no_return, ARRAY_SIZE(no_return)) == -1 && errno == EINVAL);
    errno = 0;
    REQUIRE(install(0, out_of_bounds_load, ARRAY_SIZE(out_of_bounds_load)) == -1 && errno == EINVAL);
    errno = 0;
    REQUIRE(install(0, out_of_bounds_jump, ARRAY_SIZE(out_of_bounds_jump)) == -1 && errno == EINVAL);
    errno = 0;
    REQUIRE(install(0, packet_load, ARRAY_SIZE(packet_load)) == -1 && errno == EINVAL);
    errno = 0;
    REQUIRE(install(0, allow, 0) == -1 && errno == EINVAL);
    errno = 0;
    REQUIRE(install(1u << 30, allow, ARRAY_SIZE(allow)) == -1 && errno == EINVAL);
    REQUIRE(prctl(PR_GET_SECCOMP, 0, 0, 0, 0) == initial_mode);
}

static void case_errno_action(void) {
    REQUIRE(syscall(SYS_getppid) > 0);
    no_new_privs();
    deny_getppid(0);
    REQUIRE(getppid_is_denied());
    REQUIRE(syscall(SYS_getpid) == getpid());
    REQUIRE(prctl(PR_GET_SECCOMP, 0, 0, 0, 0) == SECCOMP_MODE_FILTER);
    REQUIRE(status_field("Seccomp") == SECCOMP_MODE_FILTER);
    REQUIRE(status_field("NoNewPrivs") == 1);
}

static void case_prctl_install(void) {
    struct sock_filter filter[] = {
        LOAD_NR,
        ON_SYSCALL(SYS_getppid, SECCOMP_RET_ERRNO | EPERM),
        RETURN(SECCOMP_RET_ALLOW),
    };
    struct sock_fprog program = {.len = ARRAY_SIZE(filter), .filter = filter};
    no_new_privs();
    REQUIRE(prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &program, 0, 0) == 0);
    REQUIRE(getppid_is_denied());
}

// A later filter can add restrictions and cannot lift earlier ones.
static void case_filters_stack(void) {
    struct sock_filter second[] = {
        LOAD_NR,
        ON_SYSCALL(SYS_getuid, SECCOMP_RET_ERRNO | EACCES),
        RETURN(SECCOMP_RET_ALLOW),
    };
    no_new_privs();
    deny_getppid(0);
    REQUIRE(install(0, second, ARRAY_SIZE(second)) == 0);
    REQUIRE(getppid_is_denied());
    errno = 0;
    REQUIRE(syscall(SYS_getuid) == -1 && errno == EACCES);
    REQUIRE(status_field("Seccomp_filters") == initial_filters + 2);
}

// Filters inspect the architecture and the arguments of a system call.
static void case_inspects_arguments(void) {
    struct sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, arch)),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, AUDIT_ARCH_X86_64, 1, 0),
        RETURN(SECCOMP_RET_KILL_PROCESS),
        LOAD_NR,
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, SYS_fcntl, 0, 3),
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, args[1])),
        BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, F_GETFL, 0, 1),
        RETURN(SECCOMP_RET_ERRNO | ENOTTY),
        RETURN(SECCOMP_RET_ALLOW),
    };
    no_new_privs();
    REQUIRE(install(0, filter, ARRAY_SIZE(filter)) == 0);
    errno = 0;
    REQUIRE(fcntl(STDOUT_FILENO, F_GETFL) == -1 && errno == ENOTTY);
    REQUIRE(fcntl(STDOUT_FILENO, F_GETFD) >= 0);
}

static volatile sig_atomic_t trap_matched;

static void on_sigsys(int number, siginfo_t *info, void *context) {
    ucontext_t *user = context;
    greg_t *registers = user->uc_mcontext.gregs;
    trap_matched = number == SIGSYS && info->si_signo == SIGSYS && info->si_code == SYS_SECCOMP &&
                   info->si_errno == TRAP_DATA && info->si_syscall == SYS_getppid &&
                   info->si_arch == AUDIT_ARCH_X86_64 &&
                   (greg_t)(unsigned long)info->si_call_addr == registers[REG_RIP] &&
                   registers[REG_RAX] == SYS_getppid;
    // The handler emulates the system call by providing its result.
    registers[REG_RAX] = TRAP_RESULT;
}

static void trap_getppid(void) {
    struct sock_filter filter[] = {
        LOAD_NR,
        ON_SYSCALL(SYS_getppid, SECCOMP_RET_TRAP | TRAP_DATA),
        RETURN(SECCOMP_RET_ALLOW),
    };
    no_new_privs();
    REQUIRE(install(0, filter, ARRAY_SIZE(filter)) == 0);
}

static void case_trap_action(void) {
    struct sigaction action = {.sa_sigaction = on_sigsys, .sa_flags = SA_SIGINFO | SA_NODEFER};
    REQUIRE(sigaction(SIGSYS, &action, NULL) == 0);
    trap_getppid();
    REQUIRE(syscall(SYS_getppid) == TRAP_RESULT);
    REQUIRE(trap_matched);
}

static void expect_signal(void (*run)(void), int expected) {
    int status;
    pid_t child = fork();
    REQUIRE(child >= 0);
    if (child == 0) {
        run();
        _exit(0);
    }
    REQUIRE(waitpid(child, &status, 0) == child);
    REQUIRE(WIFSIGNALED(status) && WTERMSIG(status) == expected);
}

static void blocked_trap(void) {
    sigset_t blocked;
    struct sigaction action = {.sa_sigaction = on_sigsys, .sa_flags = SA_SIGINFO};
    REQUIRE(sigaction(SIGSYS, &action, NULL) == 0);
    REQUIRE(sigemptyset(&blocked) == 0 && sigaddset(&blocked, SIGSYS) == 0);
    REQUIRE(sigprocmask(SIG_BLOCK, &blocked, NULL) == 0);
    trap_getppid();
    syscall(SYS_getppid);
}

static void ignored_trap(void) {
    REQUIRE(signal(SIGSYS, SIG_IGN) != SIG_ERR);
    trap_getppid();
    syscall(SYS_getppid);
}

// A process cannot continue past a trapped system call by masking the signal.
static void case_trap_is_forced(void) {
    expect_signal(blocked_trap, SIGSYS);
    expect_signal(ignored_trap, SIGSYS);
}

static void kill_on_getppid(void) {
    struct sock_filter filter[] = {
        LOAD_NR,
        ON_SYSCALL(SYS_getppid, SECCOMP_RET_KILL_PROCESS),
        RETURN(SECCOMP_RET_ALLOW),
    };
    no_new_privs();
    REQUIRE(install(0, filter, ARRAY_SIZE(filter)) == 0);
    syscall(SYS_getppid);
}

static void case_kill_action(void) {
    expect_signal(kill_on_getppid, SIGSYS);
}

static void strict_mode(void) {
    static const char message[] = "";
    REQUIRE(prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT, 0, 0, 0) == 0);
    if (syscall(SYS_write, STDOUT_FILENO, message, 0) != 0) {
        syscall(SYS_exit, 1);
    }
    syscall(SYS_getppid);
    syscall(SYS_exit, 0);
}

static void case_strict_mode(void) {
    // A filtered process cannot switch to the strict mode.
    if (initial_mode == SECCOMP_MODE_DISABLED) {
        expect_signal(strict_mode, SIGKILL);
    }
}

static void case_inherited(void) {
    int status;
    no_new_privs();
    deny_getppid(0);
    pid_t child = fork();
    REQUIRE(child >= 0);
    if (child == 0) {
        if (!getppid_is_denied()) {
            _exit(1);
        }
        // The filter also survives the replacement of the program.
        execl(self_path, self_path, "--inherited", (char *)NULL);
        _exit(2);
    }
    REQUIRE(waitpid(child, &status, 0) == child);
    REQUIRE(WIFEXITED(status) && WEXITSTATUS(status) == 0);
}

static int ready_pipe[2];
static int release_pipe[2];

static void *sibling(void *unused) {
    char byte = 0;
    (void)unused;
    if (write(ready_pipe[1], &byte, 1) != 1 || read(release_pipe[0], &byte, 1) != 1) {
        return NULL;
    }
    return getppid_is_denied() ? &ready_pipe : NULL;
}

// One thread installs the filter for every thread of the process.
static void case_thread_sync(void) {
    pthread_t thread;
    void *result = NULL;
    char byte = 0;
    REQUIRE(pipe(ready_pipe) == 0 && pipe(release_pipe) == 0);
    REQUIRE(pthread_create(&thread, NULL, sibling, NULL) == 0);
    REQUIRE(read(ready_pipe[0], &byte, 1) == 1);
    no_new_privs();
    deny_getppid(SECCOMP_FILTER_FLAG_TSYNC);
    REQUIRE(write(release_pipe[1], &byte, 1) == 1);
    REQUIRE(pthread_join(thread, &result) == 0);
    REQUIRE(result != NULL);
    REQUIRE(getppid_is_denied());
    // A thread created later starts with the synchronized filter.
    REQUIRE(write(release_pipe[1], &byte, 1) == 1);
    REQUIRE(pthread_create(&thread, NULL, sibling, NULL) == 0);
    REQUIRE(pthread_join(thread, &result) == 0);
    REQUIRE(result != NULL);
}

static const struct {
    const char *name;
    void (*run)(void);
} cases[] = {
    {"support_probe", case_support_probe},
    {"requires_no_new_privs", case_requires_no_new_privs},
    {"rejects_invalid_programs", case_rejects_invalid_programs},
    {"errno_action", case_errno_action},
    {"prctl_install", case_prctl_install},
    {"filters_stack", case_filters_stack},
    {"inspects_arguments", case_inspects_arguments},
    {"trap_action", case_trap_action},
    {"trap_is_forced", case_trap_is_forced},
    {"kill_action", case_kill_action},
    {"strict_mode", case_strict_mode},
    {"inherited", case_inherited},
    {"thread_sync", case_thread_sync},
};

int main(int argc, char **argv) {
    int failed = 0;
    self_path = argv[0];
    if (argc == 2 && strcmp(argv[1], "--inherited") == 0) {
        return getppid_is_denied() ? 0 : 3;
    }
    initial_mode = prctl(PR_GET_SECCOMP, 0, 0, 0, 0);
    initial_filters = status_field("Seccomp_filters");
    if (initial_mode < 0 || initial_filters < 0) {
        puts("{\"probe\": \"seccomp\", \"status\": \"failed\", \"error\": \"seccomp state is unavailable\"}");
        return 1;
    }
    for (size_t index = 0; index < ARRAY_SIZE(cases); index++) {
        int status;
        fflush(NULL);
        pid_t child = fork();
        if (child == 0) {
            cases[index].run();
            _exit(0);
        }
        int passed = child > 0 && waitpid(child, &status, 0) == child && WIFEXITED(status) &&
                     WEXITSTATUS(status) == 0;
        failed |= !passed;
        printf("{\"probe\": \"seccomp_%s\", \"status\": \"%s\"}\n", cases[index].name,
               passed ? "passed" : "failed");
    }
    if (!failed) {
        puts("OPENSHELL_NATIVE_SECCOMP_OK");
    }
    return failed;
}
