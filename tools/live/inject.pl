#!/usr/bin/perl
# inject.pl <fifo> <session-pid>
#
# Started by entry.sh in the background. Reads byte strings from <fifo> and
# pushes each byte into its own controlling tty with TIOCSTI, as if typed.
# It only ever opens /dev/tty, so it can type into no tty but its session's.
# It exits when <session-pid> (entry.sh, then the command it exec'd) is gone.
use strict;
use warnings;
use Fcntl;

# _IOW('t', 114, char) from the macOS SDK's sys/ttycom.h:
# IOC_IN 0x80000000 | sizeof(char) << 16 | 't' << 8 | 114.
use constant TIOCSTI => 0x80017472;

my ($fifo, $pid) = @ARGV;
die "usage: inject.pl <fifo> <session-pid>\n" unless defined $pid;
$| = 1;

# A background process that touches its tty may get SIGTTOU; the kernel only
# stops it for that while the signal is not ignored.
$SIG{TTOU} = 'IGNORE';

open(my $tty, '+<', '/dev/tty') or die "inject.pl: open /dev/tty: $!\n";
# O_RDWR keeps a writer on the FIFO, so writers closing never reads as EOF.
sysopen(my $in, $fifo, O_RDWR) or die "inject.pl: open $fifo: $!\n";
print "inject.pl: ready, pid $$, pgrp ", getpgrp(), "\n";

my $rin = '';
vec($rin, fileno($in), 1) = 1;
while (kill 0, $pid) {
    my $n = select(my $rout = $rin, undef, undef, 1);
    next unless $n > 0;
    my $got = sysread($in, my $buf, 4096);
    last unless $got;
    for my $byte (split //, $buf) {
        unless (ioctl($tty, TIOCSTI, $byte)) {
            print "inject.pl: TIOCSTI failed: errno ", $! + 0, " ($!)\n";
            exit 1;
        }
    }
    print "inject.pl: injected $got byte(s)\n";
}
unlink $fifo;
