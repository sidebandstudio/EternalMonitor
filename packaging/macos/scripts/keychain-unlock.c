// em-keychain-unlock KEYCHAIN PASSWORD_FILE
// em-keychain-unlock --status KEYCHAIN
//
// Unlocks KEYCHAIN with the password read from PASSWORD_FILE, so the
// password never appears in any process's arguments or environment
// (`security unlock-keychain -p` would put it in argv). Trailing newlines are
// dropped, as `$(cat file)` did. --status prints "locked" or "unlocked" for
// the calling security session. build-app.sh builds this into target/macos/tools/.
#include <Security/Security.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

#pragma clang diagnostic ignored "-Wdeprecated-declarations"

int main(int argc, char **argv) {
    SecKeychainRef keychain = NULL;
    if (argc == 3 && strcmp(argv[1], "--status") == 0) {
        SecKeychainStatus status = 0;
        OSStatus err = SecKeychainOpen(argv[2], &keychain);
        if (err == errSecSuccess) err = SecKeychainGetStatus(keychain, &status);
        if (keychain) CFRelease(keychain);
        if (err != errSecSuccess) { fprintf(stderr, "em-keychain-unlock: %s: OSStatus %d\n", argv[2], (int)err); return 1; }
        puts(status & kSecUnlockStateStatus ? "unlocked" : "locked");
        return 0;
    }
    if (argc != 3) {
        fprintf(stderr, "usage: em-keychain-unlock KEYCHAIN PASSWORD_FILE | --status KEYCHAIN\n");
        return 64;
    }
    char password[1024];
    int fd = open(argv[2], O_RDONLY | O_CLOEXEC);
    if (fd < 0) { perror("em-keychain-unlock: password file"); return 66; }
    size_t length = 0;
    ssize_t n;
    while (length < sizeof password && (n = read(fd, password + length, sizeof password - length)) > 0) length += (size_t)n;
    close(fd);
    if (length == sizeof password) {
        memset_s(password, sizeof password, 0, sizeof password);
        fprintf(stderr, "em-keychain-unlock: password file too long\n");
        return 65;
    }
    while (length > 0 && password[length - 1] == '\n') length--;
    OSStatus err = SecKeychainOpen(argv[1], &keychain);
    if (err == errSecSuccess) err = SecKeychainUnlock(keychain, (UInt32)length, password, true);
    memset_s(password, sizeof password, 0, sizeof password);
    if (keychain) CFRelease(keychain);
    if (err != errSecSuccess) { fprintf(stderr, "em-keychain-unlock: %s: OSStatus %d\n", argv[1], (int)err); return 1; }
    return 0;
}
