# Compare captured GPU buffers without loading both heaps into memory.
# Emits an object; pipe to ConvertTo-Json -Depth 5 to retain a report outside
# the repository. Offsets in the result are relative to the buffer file.
param(
    [Parameter(Mandatory)][string]$Good,
    [Parameter(Mandatory)][string]$Bad,
    [long]$Offset = 0,
    # Zero compares everything from Offset to the longer file's end.
    [long]$Length = 0,
    [ValidateRange(1, 256)][int]$Samples = 16
)
$ErrorActionPreference = 'Stop'
if ($Offset -lt 0 -or $Length -lt 0 -or $Offset % 4 -or $Length % 4) {
    throw 'Offset and Length must be nonnegative multiples of four.'
}
$goodPath = (Resolve-Path -LiteralPath $Good).Path
$badPath = (Resolve-Path -LiteralPath $Bad).Path
if (-not ('VictoryBufferCompare' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Collections.Generic;

public sealed class VictoryBufferCompare {
    public sealed class Word {
        public long Offset;
        public string GoodHex, BadHex;
        public string GoodFloat, BadFloat;
    }
    public sealed class Result {
        public long GoodLength, BadLength, Offset, ComparedBytes;
        public long DifferentBytes, DifferentDwords;
        public long FirstDifferentByte = -1;
        public long[] Changed64KiBPages;
        public Word[] FirstDwords;
    }
    static int Fill(FileStream stream, byte[] data, int count) {
        int got = 0, n;
        while (got < count && (n = stream.Read(data, got, count - got)) > 0) got += n;
        return got;
    }
    public static Result Compare(string good, string bad, long offset, long length, int samples) {
        using (var a = File.OpenRead(good))
        using (var b = File.OpenRead(bad)) {
            long end = Math.Max(a.Length, b.Length);
            if (offset > end) throw new ArgumentOutOfRangeException("offset");
            if (length > 0) end = offset + Math.Min(length, end - offset);
            var result = new Result { GoodLength = a.Length, BadLength = b.Length,
                Offset = offset, ComparedBytes = end - offset };
            a.Position = b.Position = offset;
            var aa = new byte[65536];
            var bb = new byte[65536];
            var words = new List<Word>();
            var pages = new SortedSet<long>();
            for (long at = offset; at < end; at += aa.Length) {
                int n = (int)Math.Min(aa.Length, end - at);
                int na = Fill(a, aa, n), nb = Fill(b, bb, n);
                for (int i = 0; i < n; i += 4) {
                    bool changed = false;
                    for (int j = i; j < Math.Min(i + 4, n); ++j) {
                        if ((j < na) == (j < nb) && (j >= na || aa[j] == bb[j])) continue;
                        changed = true;
                        ++result.DifferentBytes;
                        if (result.FirstDifferentByte < 0) result.FirstDifferentByte = at + j;
                        pages.Add((at + j) / 65536);
                    }
                    if (!changed) continue;
                    ++result.DifferentDwords;
                    if (words.Count >= samples) continue;
                    bool hasA = i + 4 <= na, hasB = i + 4 <= nb;
                    words.Add(new Word {
                        Offset = at + i,
                        GoodHex = hasA ? BitConverter.ToUInt32(aa, i).ToString("x8") : "<missing/partial>",
                        BadHex = hasB ? BitConverter.ToUInt32(bb, i).ToString("x8") : "<missing/partial>",
                        GoodFloat = hasA ? BitConverter.ToSingle(aa, i).ToString("R", System.Globalization.CultureInfo.InvariantCulture) : "",
                        BadFloat = hasB ? BitConverter.ToSingle(bb, i).ToString("R", System.Globalization.CultureInfo.InvariantCulture) : ""
                    });
                }
            }
            result.Changed64KiBPages = new long[pages.Count];
            pages.CopyTo(result.Changed64KiBPages);
            result.FirstDwords = words.ToArray();
            return result;
        }
    }
}
'@
}
[VictoryBufferCompare]::Compare($goodPath, $badPath, $Offset, $Length, $Samples)
