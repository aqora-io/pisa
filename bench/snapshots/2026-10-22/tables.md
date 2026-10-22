# Environment snapshots at Mathlib scale (2026-10-22)

Library: the import closure of `Physlib.QuantumMechanics`, 86 modules and 21022 lines, one module per chunk. Header: `import Mathlib / import Lean.Elab.Command / import Physlib.Meta.TODO.Basic / import Physlib.Meta.Informal.Basic / import Physlib.Meta.Basic / import Physlib.Meta.Linters.Sorry`. Measured with `scripts/bench_snapshots.py` in the worker image, driving the REPL directly; raw numbers in `raw.json`.

| Measurement | Result |
| --- | --- |
| M0 header import | 3.4 s, 6563 MB |
| M1 resend 25% (5492 lines) as one command | 63.4 s |
| M1 resend 50% (10688 lines) as one command | 125.9 s |
| M1 resend 75% (15857 lines) as one command | 173.1 s |
| M1 resend 100% (21022 lines) as one command | 258.1 s |
| M2 replay, 86 chunks chained (21022 lines; 7 with Lean errors) | 268.3 s in total |
| M2 one chunk on its base (a statement-sized hit) | median 1.0 s, p90 8.6 s, max 33.5 s |
| M3 trivial lemma on the final env (10×) | median 17 ms, all ok: True; mid-chain env ok: True |
| Memory after the chain | 7907 MB (+1344 MB over the header, 64 MB per 1k lines) |
| M4 axiom report on the final env | whole: 6.2 s, 2280 declarations; scoped to the last chunk: 0.1 s, 31 |
| M5 module builds, 86 of 86 | 257.4 s in total, including about 1.1 s of Lake overhead per build |
| M5 import header + 25 leaf modules | 3.3 s, 6609 MB, errors: 0; trivial lemma median 5 ms |

## Per-chunk replay

| # | Module | Lines | Seconds | RSS (MB) | Errors |
| ---: | --- | ---: | ---: | ---: | ---: |
| 1 | `Physlib.StatisticalMechanics.BoltzmannConstant` | 47 | 0.0 | 6585 | 0 |
| 2 | `Physlib.Thermodynamics.Temperature.Basic` | 235 | 0.6 | 6620 | 0 |
| 3 | `Physlib.Relativity.SpeedOfLight` | 91 | 0.1 | 6626 | 0 |
| 4 | `Physlib.QuantumMechanics.PlanckConstant` | 65 | 0.1 | 6628 | 0 |
| 5 | `Physlib.QuantumMechanics.Blackbody.PlancksLaw` | 258 | 0.5 | 6635 | 0 |
| 6 | `Physlib.Mathematics.ForMathlib.OneParameterSubgroups.Basic` | 186 | 1.4 | 6666 | 0 |
| 7 | `Physlib.Mathematics.ForMathlib.OneParameterSubgroups.Unitary` | 219 | 6.8 | 6714 | 0 |
| 8 | `Physlib.QuantumMechanics.FiniteTarget` | 85 | 0.5 | 6716 | 0 |
| 9 | `Physlib.Mathematics.InnerProductSpace.Submodule` | 128 | 0.9 | 6723 | 0 |
| 10 | `Physlib.Mathematics.ForMathlib.LinearPMap` | 394 | 3.3 | 6773 | 0 |
| 11 | `Physlib.QuantumMechanics.Operators.Unbounded` | 999 | 20.1 | 7097 | 0 |
| 12 | `Physlib.SpaceAndTime.Space.Basic` | 287 | 0.7 | 7087 | 0 |
| 13 | `Physlib.SpaceAndTime.Space.Origin` | 64 | 0.1 | 7088 | 0 |
| 14 | `Physlib.SpaceAndTime.Space.Module` | 761 | 10.1 | 7092 | 3 |
| 15 | `Physlib.QuantumMechanics.HilbertSpaces.SpaceD.Basic` | 356 | 1.8 | 7095 | 0 |
| 16 | `Physlib.QuantumMechanics.HilbertSpaces.SpaceD.SchwartzSubmodule` | 176 | 1.8 | 7095 | 0 |
| 17 | `Physlib.Mathematics.Distribution.TemperedDistribution` | 668 | 13.9 | 7107 | 0 |
| 18 | `Physlib.Relativity.Tensors.RealTensor.Vector.Basic` | 473 | 3.9 | 7115 | 0 |
| 19 | `Physlib.SpaceAndTime.Space.Derivatives.Basic` | 505 | 5.4 | 7152 | 0 |
| 20 | `Physlib.QuantumMechanics.Operators.Momentum` | 168 | 5.4 | 7132 | 0 |
| 21 | `Physlib.QuantumMechanics.FreeParticle.Basic` | 95 | 0.3 | 7137 | 0 |
| 22 | `Physlib.QuantumMechanics.Operators.Multiplication` | 593 | 21.0 | 7214 | 2 |
| 23 | `Physlib.QuantumMechanics.HarmonicOscillator.Basic` | 352 | 3.4 | 7212 | 0 |
| 24 | `Physlib.Mathematics.InnerProductSpace.Gaussian` | 186 | 2.2 | 7209 | 0 |
| 25 | `Physlib.Mathematics.ForMathlib.HasTemperateGrowth` | 37 | 0.1 | 7210 | 0 |
| 26 | `Physlib.Mathematics.KroneckerDelta.Basic` | 216 | 0.7 | 7213 | 0 |
| 27 | `Physlib.QuantumMechanics.HilbertSpaces.SpaceD.PolyBddSchwartzSubmodule` | 311 | 3.6 | 7215 | 0 |
| 28 | `Physlib.SpaceAndTime.Space.Norm.Regularized` | 87 | 0.2 | 7216 | 0 |
| 29 | `Physlib.SpaceAndTime.Space.Integrals.Basic` | 264 | 1.5 | 7214 | 0 |
| 30 | `Physlib.SpaceAndTime.Space.Integrals.RadialAngularMeasure` | 260 | 2.3 | 7215 | 0 |
| 31 | `Physlib.SpaceAndTime.Time.Basic` | 88 | 0.2 | 7218 | 0 |
| 32 | `Physlib.SpaceAndTime.Time.InnerProductSpace` | 430 | 1.5 | 7225 | 0 |
| 33 | `Physlib.SpaceAndTime.Space.IsDistBounded` | 965 | 11.4 | 7272 | 0 |
| 34 | `Physlib.SpaceAndTime.Space.Integrals.NormPow` | 204 | 1.9 | 7271 | 0 |
| 35 | `Physlib.QuantumMechanics.Operators.Position` | 435 | 4.9 | 7279 | 0 |
| 36 | `Physlib.QuantumMechanics.Operators.Commutation` | 242 | 6.4 | 7305 | 0 |
| 37 | `Physlib.QuantumMechanics.HarmonicOscillator.LadderOperators` | 238 | 6.4 | 7313 | 0 |
| 38 | `Physlib.QuantumMechanics.HarmonicOscillator.NumberOperator` | 304 | 5.8 | 7327 | 0 |
| 39 | `Physlib.QuantumMechanics.HilbertSpaces.OneDimension.Basic` | 156 | 0.8 | 7322 | 0 |
| 40 | `Physlib.QuantumMechanics.HilbertSpaces.OneDimension.SchwartzSubmodule` | 60 | 0.4 | 7322 | 0 |
| 41 | `Physlib.QuantumMechanics.Operators.OneDimension.Unbounded` | 79 | 0.7 | 7323 | 0 |
| 42 | `Physlib.QuantumMechanics.Operators.OneDimension.Parity` | 118 | 0.9 | 7323 | 0 |
| 43 | `Physlib.QuantumMechanics.HilbertSpaces.OneDimension.PlaneWaves` | 55 | 0.2 | 7321 | 0 |
| 44 | `Physlib.QuantumMechanics.Operators.OneDimension.Momentum` | 148 | 1.9 | 7321 | 0 |
| 45 | `Physlib.QuantumMechanics.HilbertSpaces.OneDimension.PositionStates` | 50 | 0.1 | 7323 | 0 |
| 46 | `Physlib.Mathematics.Distribution.PowMul` | 142 | 1.2 | 7330 | 0 |
| 47 | `Physlib.QuantumMechanics.Operators.OneDimension.Position` | 111 | 0.4 | 7333 | 0 |
| 48 | `Physlib.QuantumMechanics.HarmonicOscillator.OneDimension.Basic` | 216 | 1.1 | 7345 | 0 |
| 49 | `Physlib.Mathematics.SpecialFunctions.PhysHermite` | 501 | 3.9 | 7369 | 0 |
| 50 | `Physlib.QuantumMechanics.HarmonicOscillator.OneDimension.Eigenfunction` | 303 | 1.9 | 7377 | 0 |
| 51 | `Physlib.QuantumMechanics.HarmonicOscillator.Eigenstates` | 391 | 3.3 | 7420 | 0 |
| 52 | `Physlib.QuantumMechanics.HilbertSpaces.OneDimension.Gaussians` | 216 | 0.8 | 7414 | 0 |
| 53 | `Physlib.QuantumMechanics.HarmonicOscillator.OneDimension.Completeness` | 470 | 2.6 | 7446 | 0 |
| 54 | `Physlib.QuantumMechanics.HarmonicOscillator.OneDimension.TISE` | 210 | 1.6 | 7446 | 0 |
| 55 | `Physlib.QuantumMechanics.HarmonicOscillator.OneDimension.Examples` | 74 | 0.2 | 7440 | 0 |
| 56 | `Physlib.QuantumMechanics.HilbertSpaces.FiniteTarget.Basic` | 161 | 0.7 | 7444 | 0 |
| 57 | `Physlib.QuantumMechanics.HilbertSpaces.SpaceD.DirichletSubmodule` | 93 | 0.6 | 7443 | 0 |
| 58 | `Physlib.QuantumMechanics.HilbertSpaces.SpaceD.Fourier` | 97 | 0.5 | 7453 | 0 |
| 59 | `Physlib.QuantumMechanics.HilbertSpaces.SpaceD.MomentumStates` | 65 | 0.3 | 7448 | 0 |
| 60 | `Physlib.QuantumMechanics.HilbertSpaces.SpaceD.PositionStates` | 61 | 0.1 | 7448 | 0 |
| 61 | `Physlib.QuantumMechanics.HilbertSpaces.SpaceD.SobolevSubmodule` | 84 | 0.7 | 7453 | 0 |
| 62 | `Physlib.QuantumMechanics.HilbertSpaces.TensorProducts.CompleteTensorProduct` | 275 | 3.6 | 7466 | 0 |
| 63 | `Physlib.QuantumMechanics.SpaceDQuantumSystem` | 156 | 1.1 | 7468 | 0 |
| 64 | `Physlib.QuantumMechanics.Hydrogen.Basic` | 93 | 0.5 | 7466 | 0 |
| 65 | `Physlib.Mathematics.Modules.CrossProductMatrix` | 73 | 0.5 | 7468 | 0 |
| 66 | `Physlib.QuantumMechanics.Operators.AngularMomentum` | 390 | 8.6 | 7652 | 0 |
| 67 | `Physlib.QuantumMechanics.Hydrogen.LaplaceRungeLenzVector` | 540 | 33.5 | 7899 | 2 |
| 68 | `Physlib.QuantumMechanics.Hydrogen.Rydberg` | 377 | 1.3 | 7824 | 0 |
| 69 | `Physlib.QuantumMechanics.InfiniteSquareWell.Basic` | 120 | 0.2 | 7826 | 0 |
| 70 | `Physlib.Mathematics.LeviCivita.Basic` | 186 | 0.3 | 7826 | 0 |
| 71 | `Physlib.QuantumMechanics.Operators.AngularMomentum.DimThree` | 401 | 16.9 | 7840 | 3 |
| 72 | `Physlib.QuantumMechanics.Operators.StateObservables.ExpectedValue` | 105 | 0.4 | 7825 | 0 |
| 73 | `Physlib.QuantumMechanics.Operators.StateObservables.IsEigenvector` | 42 | 0.1 | 7825 | 0 |
| 74 | `Physlib.QuantumMechanics.Operators.StateObservables.Variance` | 201 | 0.8 | 7826 | 1 |
| 75 | `Physlib.QuantumMechanics.Operators.Covariance` | 104 | 0.3 | 7826 | 0 |
| 76 | `Physlib.QuantumMechanics.Operators.Examples` | 30 | 0.0 | 7826 | 0 |
| 77 | `Physlib.QuantumMechanics.Operators.OneDimension.Commutation` | 62 | 0.3 | 7827 | 0 |
| 78 | `Physlib.QuantumMechanics.Operators.SpectralTheory.Basic` | 888 | 14.9 | 7902 | 4 |
| 79 | `Physlib.QuantumMechanics.Operators.SpectralTheory.Symmetric` | 251 | 2.1 | 7902 | 0 |
| 80 | `Physlib.QuantumMechanics.Operators.SpectralTheory.SelfAdjoint` | 136 | 0.7 | 7900 | 0 |
| 81 | `Physlib.QuantumMechanics.Operators.SpectralTheory.SpectralMeasure` | 119 | 1.0 | 7900 | 0 |
| 82 | `Physlib.QuantumMechanics.Operators.Uncertainty` | 299 | 1.5 | 7902 | 0 |
| 83 | `Physlib.Mathematics.ForMathlib.Trigonometry.Tanh` | 231 | 0.6 | 7902 | 0 |
| 84 | `Physlib.QuantumMechanics.PoschlTeller.Basic` | 307 | 1.8 | 7904 | 12 |
| 85 | `Physlib.QuantumMechanics.QuantumSystem.Basic` | 139 | 0.7 | 7904 | 0 |
| 86 | `Physlib.QuantumMechanics.RectangularBarrier.Basic` | 164 | 0.5 | 7907 | 0 |
