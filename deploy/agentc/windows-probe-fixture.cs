// Synthetic workload for windows-containment-probe.py; never a provider harness.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;

class Fixture {
    [DllImport("kernel32.dll")] static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll", SetLastError = true)] static extern IntPtr OpenProcess(uint access, bool inherit, int pid);
    [DllImport("kernel32.dll")] static extern bool CloseHandle(IntPtr handle);
    [DllImport("advapi32.dll", SetLastError = true)] static extern bool OpenProcessToken(IntPtr process, uint access, out IntPtr token);
    [DllImport("advapi32.dll", SetLastError = true)] static extern bool GetTokenInformation(IntPtr token, int kind, out int value, int size, out int needed);

    static bool Contained() {
        IntPtr token;
        if (!OpenProcessToken(GetCurrentProcess(), 8, out token)) return false;
        try {
            int value, needed;
            return GetTokenInformation(token, 29, out value, 4, out needed) && value == 1;
        } finally { CloseHandle(token); }
    }

    static void Row(List<string> rows, string key, bool value) {
        rows.Add(key + "=" + (value ? "true" : "false"));
    }

    static int Main(string[] args) {
        try { return Run(args); }
        catch (Exception error) { Console.Error.WriteLine(error.GetType().FullName); return 3; }
    }

    static int Run(string[] args) {
        if (args.Length == 1 && args[0] == "child") return Contained() ? 0 : 1;
        if (args.Length != 3) return 2;
        var rows = new List<string>();
        Row(rows, "appcontainer", Contained());
        string own = Path.Combine(Path.GetDirectoryName(args[0]), "own.txt");
        try { File.WriteAllText(own, "probe"); Row(rows, "own_write", File.ReadAllText(own) == "probe"); }
        catch { Row(rows, "own_write", false); }
        try { File.ReadAllText(args[1]); Row(rows, "host_read_denied", false); }
        catch (UnauthorizedAccessException) { Row(rows, "host_read_denied", true); }
        catch { Row(rows, "host_read_denied", false); }
        try { using (File.Open(args[1], FileMode.Open, FileAccess.Write)) {} Row(rows, "host_write_denied", false); }
        catch (UnauthorizedAccessException) { Row(rows, "host_write_denied", true); }
        catch { Row(rows, "host_write_denied", false); }
        IntPtr parent = OpenProcess(0x10 | 0x20, false, int.Parse(args[2]));
        Row(rows, "host_process_denied", parent == IntPtr.Zero && Marshal.GetLastWin32Error() == 5);
        if (parent != IntPtr.Zero) CloseHandle(parent);
        try {
            var start = new ProcessStartInfo(Process.GetCurrentProcess().MainModule.FileName, "child");
            start.UseShellExecute = false;
            using (var child = Process.Start(start)) {
                bool ended = child.WaitForExit(10000);
                Row(rows, "child_contained", ended && child.ExitCode == 0);
                if (!ended) child.Kill();
            }
        } catch { Row(rows, "child_contained", false); }
        File.WriteAllLines(args[0], rows);
        return 0;
    }
}
