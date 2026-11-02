#!/usr/bin/perl
# keys.pl: decode a key string with escapes and hand it on.
#
#   keys.pl fifo <fifo> <delay-ms> <string>   write the bytes to <fifo>
#   keys.pl json <delay-ms> <string>          print {"send": [units], "delayMs": n}
#
# Escapes: \e \t \r \n \\ \xHH. A "unit" is one key: an escape sequence
# (ESC [ ... final, ESC O x, ESC x), one UTF-8 character, or one byte. With a
# delay, units are written one at a time with that pause between them.
use strict;
use warnings;
use Fcntl;
use JSON::PP;
use Time::HiRes qw(usleep);

sub decode_escapes {
    my ($s) = @_;
    my $out = '';
    while (length $s) {
        if ($s =~ s/^\\e//)              { $out .= "\e" }
        elsif ($s =~ s/^\\t//)           { $out .= "\t" }
        elsif ($s =~ s/^\\r//)           { $out .= "\r" }
        elsif ($s =~ s/^\\n//)           { $out .= "\n" }
        elsif ($s =~ s/^\\\\//)          { $out .= "\\" }
        elsif ($s =~ s/^\\x([0-9A-Fa-f]{2})//) { $out .= chr(hex $1) }
        else { $s =~ s/^(.)//s; $out .= $1 }
    }
    return $out;
}

sub units {
    my ($bytes) = @_;
    my @u;
    while (length $bytes) {
        if ($bytes =~ s/^(\e\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e])//) { push @u, $1 }
        elsif ($bytes =~ s/^(\eO.)//s)                              { push @u, $1 }
        elsif ($bytes =~ s/^(\e.)//s)                               { push @u, $1 }
        elsif ($bytes =~ s/^([\xc0-\xf7][\x80-\xbf]+)//)            { push @u, $1 }
        else { $bytes =~ s/^(.)//s; push @u, $1 }
    }
    return @u;
}

my $mode = shift // '';
if ($mode eq 'fifo') {
    my ($fifo, $delay, $str) = @ARGV;
    die "usage: keys.pl fifo <fifo> <delay-ms> <string>\n" unless defined $str;
    # Non-blocking open fails at once (ENXIO) when no injector is reading.
    sysopen(my $fh, $fifo, O_WRONLY | O_NONBLOCK) or die "keys.pl: open $fifo: $!\n";
    my $flags = fcntl($fh, F_GETFL, 0);
    fcntl($fh, F_SETFL, $flags & ~O_NONBLOCK);
    my @u = $delay > 0 ? units(decode_escapes($str)) : (decode_escapes($str));
    for my $i (0 .. $#u) {
        syswrite($fh, $u[$i]) // die "keys.pl: write: $!\n";
        usleep($delay * 1000) if $delay > 0 && $i < $#u;
    }
    close $fh;
} elsif ($mode eq 'json') {
    my ($delay, $str) = @ARGV;
    die "usage: keys.pl json <delay-ms> <string>\n" unless defined $str;
    my $bytes = decode_escapes($str);
    my @u = $delay > 0 ? units($bytes) : ($bytes);
    # Units are bytes; decode UTF-8 so JSON carries characters.
    utf8::decode($_) for @u;
    print JSON::PP->new->ascii->canonical->encode({ send => \@u, delayMs => $delay + 0 }), "\n";
} else {
    die "usage: keys.pl fifo|json ...\n";
}
