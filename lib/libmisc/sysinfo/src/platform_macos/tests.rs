use super::*;

#[test]
fn power_output_distinguishes_charge_unknown_and_absent_battery() {
    assert_eq!(
        parse_power("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t85%; charging;"),
        (true, Some(85), true)
    );
    assert_eq!(
        parse_power(
            "Now drawing from 'Battery Power'\n -InternalBattery-0 (id=1)\t15%; discharging;"
        ),
        (true, Some(15), false)
    );
    assert_eq!(
        parse_power("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\tunknown;"),
        (true, None, true)
    );
    assert_eq!(
        parse_power("Now drawing from 'AC Power'"),
        (false, None, false)
    );
}
