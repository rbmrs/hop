// Throwaway DDC/CI probe for issue #2: talks to the external display over
// IOAVService (Apple Silicon), with checksum-validated replies and retries.
//
//   probe get <vcp-hex> [n]   read a VCP feature n times (default 1)
//   probe caps                read and print the capabilities string
//   probe set <vcp-hex> <val> write a VCP feature (0x60 switches input!)

#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOKitLib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef CFTypeRef IOAVServiceRef;
extern IOAVServiceRef IOAVServiceCreateWithService(CFAllocatorRef, io_service_t);
extern IOReturn IOAVServiceReadI2C(IOAVServiceRef, uint32_t chip, uint32_t offset, void *buf, uint32_t len);
extern IOReturn IOAVServiceWriteI2C(IOAVServiceRef, uint32_t chip, uint32_t offset, void *buf, uint32_t len);

#define DDC_CHIP 0x37
#define DDC_SRC 0x51
#define DDC_DEST 0x6E
#define DELAY_US 50000
#define RETRIES 5

static IOAVServiceRef find_external(void) {
  io_iterator_t it;
  io_registry_entry_t root = IORegistryGetRootEntry(kIOMainPortDefault);
  IORegistryEntryCreateIterator(root, kIOServicePlane, kIORegistryIterateRecursively, &it);
  io_service_t s;
  IOAVServiceRef found = NULL;
  while (!found && (s = IOIteratorNext(it))) {
    io_name_t name;
    IORegistryEntryGetName(s, name);
    if (strcmp(name, "DCPAVServiceProxy") == 0) {
      CFStringRef loc = IORegistryEntrySearchCFProperty(s, kIOServicePlane, CFSTR("Location"),
                                                        kCFAllocatorDefault, kIORegistryIterateRecursively);
      if (loc && CFStringCompare(loc, CFSTR("External"), 0) == kCFCompareEqualTo)
        found = IOAVServiceCreateWithService(kCFAllocatorDefault, s);
      if (loc) CFRelease(loc);
    }
    IOObjectRelease(s);
  }
  IOObjectRelease(it);
  return found;
}

// Sends body (without length byte or checksum) to the display.
static IOReturn ddc_write(IOAVServiceRef av, const uint8_t *body, uint8_t n) {
  uint8_t pkt[40];
  pkt[0] = 0x80 | n;
  memcpy(pkt + 1, body, n);
  uint8_t ck = DDC_DEST ^ DDC_SRC;
  for (int i = 0; i <= n; i++) ck ^= pkt[i];
  pkt[n + 1] = ck;
  return IOAVServiceWriteI2C(av, DDC_CHIP, DDC_SRC, pkt, n + 2);
}

// Validates a reply: source byte, length, checksum (seeded with 0x50).
static int reply_ok(const uint8_t *r, int max) {
  if (r[0] != DDC_DEST) return 0;
  int len = r[1] & 0x7F;
  if (!(r[1] & 0x80) || len + 3 > max) return 0;
  uint8_t ck = 0x50;
  for (int i = 0; i < len + 2; i++) ck ^= r[i];
  return ck == r[len + 2];
}

static void hexdump(const uint8_t *r, int n) {
  for (int i = 0; i < n; i++) fprintf(stderr, "%02x ", r[i]);
  fprintf(stderr, "\n");
}

static int get_vcp(IOAVServiceRef av, uint8_t vcp, int *cur, int *max, int *attempts) {
  for (int a = 1; a <= RETRIES; a++) {
    uint8_t body[] = {0x01, vcp};
    uint8_t r[12] = {0};
    if (ddc_write(av, body, 2) != kIOReturnSuccess) continue;
    usleep(DELAY_US);
    if (IOAVServiceReadI2C(av, DDC_CHIP, DDC_SRC, r, sizeof r) != kIOReturnSuccess) continue;
    if (!reply_ok(r, sizeof r) || r[2] != 0x02 || r[3] != 0x00 || r[4] != vcp) {
      fprintf(stderr, "  bad reply (attempt %d): ", a);
      hexdump(r, sizeof r);
      usleep(DELAY_US);
      continue;
    }
    *max = (r[6] << 8) | r[7];
    *cur = (r[8] << 8) | r[9];
    *attempts = a;
    return 0;
  }
  return -1;
}

static int caps(IOAVServiceRef av) {
  char out[4096] = {0};
  int off = 0;
  while (off < (int)sizeof out - 64) {
    int got = -1;
    for (int a = 1; a <= RETRIES && got < 0; a++) {
      uint8_t body[] = {0xF3, off >> 8, off & 0xFF};
      uint8_t r[38] = {0};
      if (ddc_write(av, body, 3) != kIOReturnSuccess) continue;
      usleep(DELAY_US);
      if (IOAVServiceReadI2C(av, DDC_CHIP, DDC_SRC, r, sizeof r) != kIOReturnSuccess) continue;
      int len = r[1] & 0x7F;
      if (!reply_ok(r, sizeof r) || r[2] != 0xE3 || ((r[3] << 8) | r[4]) != off) {
        usleep(DELAY_US);
        continue;
      }
      got = len - 3;
      memcpy(out + off, r + 5, got);
    }
    if (got < 0) { fprintf(stderr, "caps failed at offset %d\n", off); return -1; }
    if (got == 0) break;
    off += got;
  }
  printf("%s\n", out);
  return 0;
}

int main(int argc, char **argv) {
  if (argc < 2) { fprintf(stderr, "usage: probe get|caps|set ...\n"); return 2; }
  IOAVServiceRef av = find_external();
  if (!av) { fprintf(stderr, "no external IOAVService\n"); return 1; }

  if (!strcmp(argv[1], "get") && argc >= 3) {
    uint8_t vcp = strtol(argv[2], NULL, 16);
    int n = argc >= 4 ? atoi(argv[3]) : 1, ok = 0, retried = 0;
    for (int i = 0; i < n; i++) {
      int cur, max, att;
      if (get_vcp(av, vcp, &cur, &max, &att) == 0) {
        ok++;
        if (att > 1) retried++;
        printf("cur=%d max=%d attempts=%d\n", cur, max, att);
      } else {
        printf("failed\n");
      }
    }
    fprintf(stderr, "%d/%d ok, %d needed a retry\n", ok, n, retried);
    return ok == n ? 0 : 1;
  }
  if (!strcmp(argv[1], "caps")) return caps(av) ? 1 : 0;
  if (!strcmp(argv[1], "set") && argc >= 4) {
    uint8_t vcp = strtol(argv[2], NULL, 16);
    int v = atoi(argv[3]);
    uint8_t body[] = {0x03, vcp, v >> 8, v & 0xFF};
    IOReturn rc = ddc_write(av, body, 4);
    printf("set 0x%02x=%d rc=0x%x\n", vcp, v, rc);
    return rc == kIOReturnSuccess ? 0 : 1;
  }
  fprintf(stderr, "bad args\n");
  return 2;
}
