import io, shutil, sys
p = r"C:\winquick-lab\qemu-src\target\i386\whpx\whpx-all.c"
shutil.copyfile(p + ".bak-synicdiag", p)  # start from pristine
s=None
s = io.open(p, encoding="utf-8", newline="").read()
def sub(old, new, why):
    global s
    if old not in s: sys.exit("NOT FOUND: " + why)
    s = s.replace(old, new, 1)

helper = '''
/*
 * Lab instrument: dump every piece of Hyper-V timing state public WHP will
 * hand over, at the freeze and again after the restore, so the two can be
 * diffed byte for byte. Writes to the file named by WHPX_SYNIC_DIAG; does
 * nothing when that is unset. Not shipped.
 */
static void whpx_synic_dump(CPUState *cpu, const char *when)
{
    const char *path = getenv("WHPX_SYNIC_DIAG");
    struct whpx_state *whpx = &whpx_global;
    uint8_t blob[1024];
    UINT32 written = 0;
    HRESULT hr;
    FILE *f;
    unsigned i;
    static const struct { const char *name; WHV_REGISTER_NAME reg; } regs[] = {
        { "Tsc",            WHvX64RegisterTsc },
        { "Scontrol",       WHvRegisterScontrol },
        { "Sversion",       WHvRegisterSversion },
        { "Simp",           WHvRegisterSimp },
        { "Siefp",          WHvRegisterSiefp },
        { "Eom",            WHvRegisterEom },
        { "VpRuntime",      WHvRegisterVpRuntime },
        { "GuestOsId",      WHvRegisterGuestOsId },
        { "Hypercall",      WHvX64RegisterHypercall },
        { "VpAssistPage",   WHvRegisterVpAssistPage },
        { "ReferenceTsc",   WHvRegisterReferenceTsc },
        { "RefTscSeq",      WHvRegisterReferenceTscSequence },
    };

    if (!path) {
        return;
    }
    f = fopen(path, "a");
    if (!f) {
        return;
    }
    fprintf(f, "== vp%d %s ==\\n", cpu->cpu_index, when);
    for (i = 0; i < ARRAY_SIZE(regs); i++) {
        uint64_t v = 0;
        if (whpx_try_get_reg(cpu, regs[i].reg, &v)) {
            fprintf(f, "  %-14s %020llu  0x%016llx\\n", regs[i].name,
                    (unsigned long long)v, (unsigned long long)v);
        } else {
            fprintf(f, "  %-14s <unreadable>\\n", regs[i].name);
        }
    }
    for (i = 0; i < 16; i++) {
        uint64_t v = 0;
        if (whpx_try_get_reg(cpu, (WHV_REGISTER_NAME)(WHvRegisterSint0 + i), &v) && v != 0x10000) {
            fprintf(f, "  Sint%-10u 0x%016llx\\n", i, (unsigned long long)v);
        }
    }
    memset(blob, 0, sizeof(blob));
    hr = whp_dispatch.WHvGetVirtualProcessorState(
        whpx->partition, cpu->cpu_index,
        WHvVirtualProcessorStateTypeSynicTimerState,
        blob, sizeof(blob), &written);
    if (FAILED(hr)) {
        fprintf(f, "  SynicTimerState  <hr=0x%08lx>\\n", (unsigned long)hr);
    } else {
        fprintf(f, "  SynicTimerState  %u bytes\\n", (unsigned)written);
        for (i = 0; i < written; i += 16) {
            unsigned j;
            fprintf(f, "    %04u:", i);
            for (j = 0; j < 16 && i + j < written; j++) {
                fprintf(f, " %02x", blob[i + j]);
            }
            fprintf(f, "\\n");
        }
    }
    fclose(f);
}

'''
sub("static int whpx_vp_pre_save(void *opaque)", helper + "static int whpx_vp_pre_save(void *opaque)", "helper")
# It is called from whpx_apply_pending_hv_state, which comes earlier in the file.
sub("static void whpx_apply_pending_hv_state(CPUState *cpu)",
    "static void whpx_synic_dump(CPUState *cpu, const char *when);\n\nstatic void whpx_apply_pending_hv_state(CPUState *cpu)",
    "forward decl")
sub("""    whpx_try_get_reg(s->cpu, WHvRegisterReferenceTsc, &s->reference_tsc);
    return 0;""",
    """    whpx_try_get_reg(s->cpu, WHvRegisterReferenceTsc, &s->reference_tsc);
    whpx_synic_dump(s->cpu, "SOURCE-at-freeze");
    return 0;""", "save hook")
sub("""    p->internal_activity &= ~(uint64_t)(WHPX_ACTIVITY_HALT_SUSPEND |
                                        WHPX_ACTIVITY_IDLE_SUSPEND);
    whpx_try_set_reg(cpu, WHvRegisterInternalActivityState,
                     p->internal_activity);""",
    """    whpx_synic_dump(cpu, "DEST-before-activity");
    p->internal_activity &= ~(uint64_t)(WHPX_ACTIVITY_HALT_SUSPEND |
                                        WHPX_ACTIVITY_IDLE_SUSPEND);
    whpx_try_set_reg(cpu, WHvRegisterInternalActivityState,
                     p->internal_activity);
    whpx_synic_dump(cpu, "DEST-after-activity");""", "restore hook")
io.open(p, "w", encoding="utf-8", newline="").write(s)
print("instrumented")
