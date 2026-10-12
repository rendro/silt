// step COUNTS TRACE DELAY_MS STEPS TRACED BIN ARGS...
//
// Runs BIN freely for DELAY_MS, then stops every thread of it and
// single-steps them for STEPS instructions (ptrace), counting how often
// each address of the executable is about to run. For the TRACED steps
// after the first 2000 it also writes every register before the step.
// Then it kills BIN.
//
// COUNTS gets the executable's load address ("base <hex>"), then
// "offset count" per line (offset from that address); TRACE gets the
// registers (struct user_regs_struct, rip as it is).
//
// Linux x86_64 only. No performance counters are needed: the counts
// are exact and do not depend on the machine's load.
#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/user.h>
#include <sys/wait.h>
#include <unistd.h>

#define SIZE (64UL << 20)

static pid_t tids[1024];
static int nt;
static volatile sig_atomic_t fired;

static void on_alarm(int s) { (void)s; fired = 1; }

static int known(pid_t t) {
  for (int i = 0; i < nt; i++)
    if (tids[i] == t) return 1;
  return 0;
}

static void forget(pid_t t) {
  for (int i = 0; i < nt; i++)
    if (tids[i] == t) { tids[i] = tids[--nt]; return; }
}

// The address the executable is loaded at: the start of its first
// mapping.
static unsigned long load_address(pid_t pid) {
  char path[64], exe[4096], line[8192];
  snprintf(path, sizeof path, "/proc/%d/exe", pid);
  ssize_t n = readlink(path, exe, sizeof exe - 1);
  if (n < 0) return 0;
  exe[n] = 0;
  snprintf(path, sizeof path, "/proc/%d/maps", pid);
  FILE *maps = fopen(path, "r");
  if (!maps) return 0;
  unsigned long base = 0;
  while (fgets(line, sizeof line, maps)) {
    if (strstr(line, exe)) { base = strtoul(line, 0, 16); break; }
  }
  fclose(maps);
  return base;
}

int main(int argc, char **argv) {
  if (argc < 7) {
    fprintf(stderr, "usage: step COUNTS TRACE DELAY_MS STEPS TRACED BIN ARGS...\n");
    return 2;
  }
  const char *counts = argv[1], *trace_path = argv[2];
  int delay = atoi(argv[3]);
  long steps_wanted = atol(argv[4]), traced = atol(argv[5]);
  pid_t child = fork();
  if (!child) {
    int null = open("/dev/null", O_WRONLY);
    dup2(null, 1);
    ptrace(PTRACE_TRACEME, 0, 0, 0);
    execv(argv[6], argv + 6);
    _exit(127);
  }
  int status;
  waitpid(child, &status, 0);
  if (!WIFSTOPPED(status)) { fprintf(stderr, "step: %s did not start\n", argv[6]); return 1; }
  unsigned long base = load_address(child);
  if (!base) { fprintf(stderr, "step: no load address\n"); kill(child, SIGKILL); return 1; }
  ptrace(PTRACE_SETOPTIONS, child, 0, PTRACE_O_TRACECLONE | PTRACE_O_EXITKILL);
  tids[nt++] = child;
  struct sigaction sa;
  memset(&sa, 0, sizeof sa);
  sa.sa_handler = on_alarm;
  sigaction(SIGALRM, &sa, 0);
  struct itimerval timer = {{0, 0}, {delay / 1000, (delay % 1000) * 1000}};
  setitimer(ITIMER_REAL, &timer, 0);
  ptrace(PTRACE_CONT, child, 0, 0);
  uint32_t *count = calloc(SIZE, sizeof *count);
  FILE *trace = fopen(trace_path, "w");
  int stepping = 0, ended = 0;
  long steps = 0, outside = 0;
  for (;;) {
    if (fired && !stepping) {
      stepping = 1;
      for (int i = 0; i < nt; i++) syscall(SYS_tgkill, child, tids[i], SIGSTOP);
    }
    pid_t t = waitpid(-1, &status, __WALL);
    if (t < 0) { if (errno == EINTR) continue; break; }
    if (WIFEXITED(status) || WIFSIGNALED(status)) {
      forget(t);
      if (t == child) { ended = 1; break; }
      continue;
    }
    int sig = WSTOPSIG(status), event = status >> 16, deliver = 0;
    if (!known(t)) {
      tids[nt++] = t;                         // a new thread's first stop
    } else if (event) {                       // the clone event of its parent
    } else if (sig == SIGTRAP) {
      if (stepping) {
        struct user_regs_struct regs;
        ptrace(PTRACE_GETREGS, t, 0, &regs);
        if (regs.rip - base < SIZE) count[regs.rip - base]++; else outside++;
        if (steps >= 2000 && steps < 2000 + traced) fwrite(&regs, sizeof regs, 1, trace);
        if (++steps >= steps_wanted) break;
      }
    } else if (sig == SIGSTOP && stepping) {  // our stop
    } else {
      deliver = sig;
    }
    ptrace(stepping ? PTRACE_SINGLESTEP : PTRACE_CONT, t, 0, deliver);
  }
  if (!ended) kill(child, SIGKILL);
  fclose(trace);
  FILE *out = fopen(counts, "w");
  fprintf(out, "base %lx\n", base);
  for (unsigned long i = 0; i < SIZE; i++)
    if (count[i]) fprintf(out, "%lx %u\n", i, count[i]);
  fclose(out);
  if (ended || steps < steps_wanted) {
    fprintf(stderr, "step: the program ended before %ld instructions were stepped (%ld)\n", steps_wanted, steps);
    return 1;
  }
  fprintf(stderr, "step: %ld instructions stepped, %ld outside the executable\n", steps, outside);
  return 0;
}
