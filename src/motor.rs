use embedded_hal::digital::{OutputPin, StatefulOutputPin};

pub struct MotorDriver<
    AL: OutputPin + StatefulOutputPin,
    AH: OutputPin + StatefulOutputPin,
    BL: OutputPin + StatefulOutputPin,
    BH: OutputPin + StatefulOutputPin,
    CL: OutputPin + StatefulOutputPin,
    CH: OutputPin + StatefulOutputPin,
> {
    pub a: Phase<AL, AH>,
    pub b: Phase<BL, BH>,
    pub c: Phase<CL, CH>,
    pub comm_state: CommutationState,
}

#[derive(Debug)]
pub enum ControlState {
    Idle,
    Brake,
    Forward,
    Reverse,
}

#[derive(Debug)]
pub enum CommutationState {
    AB,
    AC,
    BC,
    BA,
    CA,
    CB,
}

impl CommutationState {
    pub fn next(&self) -> Self {
        match self {
            CommutationState::AB => CommutationState::AC,
            CommutationState::AC => CommutationState::BC,
            CommutationState::BC => CommutationState::BA,
            CommutationState::BA => CommutationState::CA,
            CommutationState::CA => CommutationState::CB,
            CommutationState::CB => CommutationState::AB,
        }
    }

    fn previous(&self) -> Self {
        match self {
            CommutationState::AB => CommutationState::CB,
            CommutationState::AC => CommutationState::AB,
            CommutationState::BC => CommutationState::AC,
            CommutationState::BA => CommutationState::BC,
            CommutationState::CA => CommutationState::BA,
            CommutationState::CB => CommutationState::CA,
        }
    }
}

impl<
        AL: OutputPin + StatefulOutputPin,
        AH: OutputPin + StatefulOutputPin,
        BL: OutputPin + StatefulOutputPin,
        BH: OutputPin + StatefulOutputPin,
        CL: OutputPin + StatefulOutputPin,
        CH: OutputPin + StatefulOutputPin,
    > MotorDriver<AL, AH, BL, BH, CL, CH>
{
    pub fn new(a: Phase<AL, AH>, b: Phase<BL, BH>, c: Phase<CL, CH>) -> Self {
        Self {
            a,
            b,
            c,
            comm_state: CommutationState::AB,
        }
    }

    pub fn step(&mut self, direction: bool) {
        self.comm_state = match direction {
            true => self.comm_state.next(),
            false => self.comm_state.previous(),
        };
        //self.comm_state = self.comm_state.next();

        match self.comm_state {
            CommutationState::AB => {
                self.a.set_high();
                self.b.set_low();
                self.c.set_floating();
            }
            CommutationState::AC => {
                self.a.set_high();
                self.b.set_floating();
                self.c.set_low();
            }
            CommutationState::BC => {
                self.a.set_floating();
                self.b.set_high();
                self.c.set_low();
            }
            CommutationState::BA => {
                self.a.set_low();
                self.b.set_high();
                self.c.set_floating();
            }
            CommutationState::CA => {
                self.a.set_low();
                self.b.set_floating();
                self.c.set_high();
            }
            CommutationState::CB => {
                self.a.set_floating();
                self.b.set_low();
                self.c.set_high();
            }
        }
    }

    pub fn set_idle(&mut self) {
        self.a.set_floating();
        self.b.set_floating();
        self.c.set_floating();
    }
}

pub struct Phase<L: OutputPin + StatefulOutputPin, H: OutputPin + StatefulOutputPin> {
    low_gate: L,
    high_gate: H,
}

impl<L: OutputPin + StatefulOutputPin, H: OutputPin + StatefulOutputPin> Phase<L, H> {
    pub fn new(mut low_gate: L, mut high_gate: H) -> Self {
        high_gate.set_low();
        low_gate.set_low();

        Self {
            low_gate,
            high_gate,
        }
    }

    fn set_floating(&mut self) {
        self.high_gate.set_low();
        self.low_gate.set_low();
    }

    /// Set the phase to VIN
    fn set_high(&mut self) {
        self.low_gate.set_low();

        self.high_gate.set_high();
    }

    /// Set the phase to ground
    fn set_low(&mut self) {
        self.high_gate.set_low();
        self.low_gate.set_high();
    }

    /*
    fn is_set_high(&mut self) -> bool {
        self.high_gate.is_set_high() && self.low_gate.is_set_low()
    }

    fn is_set_low(&mut self) -> bool {
        self.low_gate.is_set_high() && self.high_gate.is_set_low()
    }
    */
}
