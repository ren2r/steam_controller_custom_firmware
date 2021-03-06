use lpc11uxx::SYSCON;

static WDT_OSC_RATE: [u32; 16] = [
           0x00000000,
           0x000927C0,
           0x00100590,
           0x00155CC0,
           0x001AB3F0,
           0x00200B20,
           0x00249F00,
           0x002932E0,
           0x002DC6C0,
           0x00319750,
           0x003567E0,
           0x00393870,
           0x003D0900,
           0x00401640,
           0x00432380,
           0x004630C0,
];


pub fn get_main_clock_rate(syscon: &SYSCON) -> u32 {
    use lpc11uxx::syscon::mainclksel::SEL_A::*;
    match syscon.mainclksel.read().sel().variant() {
        IRC_OSCILLATOR => {
            12_000_000
        },
        PLL_INPUT => {
            12_000_000
        },
        WATCHDOG_OSCILLATOR => {
            let wdtoscctrl = syscon.wdtoscctrl.read();
            WDT_OSC_RATE[wdtoscctrl.freqsel().bits() as usize] / (u32::from(wdtoscctrl.divsel().bits()) * 2 + 2)
        },
        PLL_OUTPUT => {
            let pll_reg = syscon.syspllctrl.read().msel().bits();
            let input_rate = 12_000_000;
            (u32::from(pll_reg) + 1) * input_rate
        }
    }
}

pub fn get_system_clock_rate(syscon: &SYSCON) -> u32 {
    let clock_rate = get_main_clock_rate(syscon);
    let ticks_per_sec = syscon.sysahbclkdiv.read().div().bits();

    clock_rate / u32::from(ticks_per_sec)
}