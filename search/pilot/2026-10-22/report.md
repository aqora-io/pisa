# Informalization pilot, 2026-10-22

- Environment: `v4.35.0-da683984`
- Model: `claude-haiku-5-5`, effort `low`, JSON-schema output; the instructions and glossary are prompt-cached
- Sample: 508 declarations from 45 Physlib modules (about 500 requested, whole modules stratified by area, seed 1)
- 0 Physlib informal results in the sample took their docstring as the description (no model call)

## Cost

| | |
|---|---|
| Calls | 508 (507 described, 1 failed) |
| Input tokens: uncached / cache write / cache read | 889601 / 0 / 2412492 |
| Output tokens (with thinking) | 192568 |
| Cost | $0.209 ($0.00041 per description) |
| Wall time | 165 s at concurrency 8 |

### Full run estimate

Declarations that need a description and had none stored before this run (LeanSearch v2 covers much of Mathlib), at this pilot's mean cost per description. The Message Batches API halves it.

| Library | Declarations | Messages API | Batches API |
|---|---:|---:|---:|
| Batteries | 3460 | $1.43 | $0.71 |
| Init | 31717 | $13.10 | $6.55 |
| Mathlib | 54637 | $22.56 | $11.28 |
| Physlib | 10488 | $4.33 | $2.17 |
| QuantumInfo | 9 | $0.00 | $0.00 |
| Std | 26994 | $11.15 | $5.57 |
| **All** | **127305** | **$52.57** | **$26.29** |

## Failures

- `SuperSymmetry.SU5.ChargeSpectrum.minimallyAllowsTerm_iff_mem_minimallyAllowsTermOfFinset`: empty description

## Checks

| Check | Passed |
|---|---:|
| LaTeX balanced and KaTeX-compatible | 497/507 (98%) |
| Cited Lean names are used by the declaration | 499/507 (98%) |
| No cited Lean name is unknown | 507/507 (100%) |
| All checks | 489/507 (96%) |
| Has a LaTeX formula | 366/507 (72%) |

Failing checks (first 15):

- `ClassicalMechanics.HarmonicOscillatorWithDim`: cites `ClassicalMechanics.HarmonicOscillator`, which it doesn't use
- `MSSMACC.AnomalyFreePerp`: cites `ACCSystemLinear.LinSols`, which it doesn't use
- `Fermion.Dirac.Slash.slash`: \slashed isn't a KaTeX command; \slashed isn't a KaTeX command
- `Lorentz.Vector.tensor_basis_repr_toTensor_prod_apply`: cites `Lorentz.Vector.basis.tensorProduct`, which it doesn't use
- `Physlib.HTMLNote.ofInformal`: cites `InformalLemma`, which it doesn't use; cites `InformalDefinition`, which it doesn't use
- `CreateAnnihilate.create`: cites `CreateAnnihilate.annihilate`, which it doesn't use
- `CreateAnnihilate.annihilate`: cites `CreateAnnihilate.create`, which it doesn't use
- `SpectralMeasure.toVectorMeasure`: \toL isn't a KaTeX command
- `Fermion.Dirac.Slash.slash_zero`: \slashed isn't a KaTeX command; \slashed isn't a KaTeX command; \slashed isn't a KaTeX command
- `Fermion.Dirac.Slash.slash_eq_sum`: \slashed isn't a KaTeX command; \slashed isn't a KaTeX command
- `Fermion.Dirac.Slash.slashMulLeft`: \slashed isn't a KaTeX command; \slashed isn't a KaTeX command; \slashed isn't a KaTeX command
- `Fermion.Dirac.Slash.slash_add`: \slashed isn't a KaTeX command; \slashed isn't a KaTeX command
- `Fermion.Dirac.Slash.slash_smul`: \slashed isn't a KaTeX command; \slashed isn't a KaTeX command
- `Lorentz.Vector.toTensor_basis_eq_tensor_basis`: cites `Lorentz.Vector.indexEquiv.symm`, which it doesn't use
- `SMRHN.chargeMap_apply`: cites `SMνCharges.toSpeciesEquiv.symm`, which it doesn't use

## Discrimination

17/20 descriptions picked out their declaration from it and its three nearest neighbours in the dense lane, names hidden, judged by `claude-haiku-5-5` ($0.002).
Missed: `FluidDynamics.NavierStokes`, `Physlib.HTMLNote.ofInformal`, `RigidBody.rotationalKineticEnergy`.

## Samples (40)

### `Basis.conj`

def in `Physlib.Mathematics.Modules.ConjModule`

```lean
Basis.conj.{u_1, u_2, u_3} {k : Type u_1} [CommRing k] [StarRing k] {M : Type u_2} [AddCommGroup M] [Module k M]
  {ι : Type u_3} (b : Module.Basis ι k M) : Module.Basis ι k (ConjModule M)
```

Docstring: A basis of `M` transported to a basis of `ConjModule M`: the same basis vectors, with coordinates conjugated (`(Basis.conj b).repr v = star ∘ b.repr v`).

**Conjugate basis of the conjugate module**. Given a basis $b$ of a module $M$ over a commutative star ring $k$, indexed by $\iota$, `Basis.conj` is the basis of `ConjModule M` with the same basis vectors as $b$. Its coordinates are the coordinates of $b$ conjugated: the representation of $v$ is $\overline{b.repr\, v}$ coordinatewise.

Queries: “basis of conjugate module with conjugated coordinates” · “How do you transport a basis to the conjugate module over a star ring?” · “Basis.conj ConjModule star coordinates repr”

### `ClassicalMechanics.DampedHarmonicOscillator.trajectory_apply_zero`

theorem in `Physlib.ClassicalMechanics.DampedHarmonicOscillator.Solution`

```lean
ClassicalMechanics.DampedHarmonicOscillator.trajectory_apply_zero (S : ClassicalMechanics.DampedHarmonicOscillator)
  (IC : ClassicalMechanics.DampedHarmonicOscillator.InitialConditions) : S.trajectory IC 0 = IC.x₀
```

Docstring: The selected trajectory has initial position `IC.x₀`.

**Initial position of the damped harmonic oscillator trajectory**. The solution trajectory of a damped harmonic oscillator $S$ selected for initial conditions $IC$ takes the value $x_0$ at time $t = 0$, where $x_0$ is the initial position in $IC$. The lemma holds for all damping regimes (underdamped, critically damped and overdamped), since `S.trajectory IC` is chosen by the sign of the discriminant.

$$
x(0) = x_0
$$

Queries: “damped harmonic oscillator trajectory at time zero equals initial position” · “x(0) = x_0 damped oscillator solution initial condition” · “damped oscillator trajectory initial position lemma underdamped critically damped overdamped”

### `ConjTensorSpecies.bar_involution`

theorem in `Physlib.Relativity.Tensors.Conjugation.Basic`

```lean
ConjTensorSpecies.bar_involution {k : Type} [CommRing k] [StarRing k] {C G : Type} [Group G] {V : C → Type}
  [(c : C) → AddCommGroup (V c)] [(c : C) → Module k (V c)] {basisIdx : C → Type} [(c : C) → Fintype (basisIdx c)]
  [(c : C) → DecidableEq (basisIdx c)] {rep : (c : C) → Representation k G (V c)}
  {b : (c : C) → Module.Basis (basisIdx c) k (V c)} (self : ConjTensorSpecies k C G V basisIdx rep b) :
  Function.Involutive self.bar
```

Docstring: `bar` is an involution.

**Conjugate colour map is an involution**. For a conjugation structure on a tensor species, the map `ConjTensorSpecies.bar` sending each index colour $c$ to its conjugate colour is involutive, so applying it twice gives back the original colour. This is the statement that conjugating a tensor index twice returns the original index colour.

$$
\overline{\overline{c}} = c \text{ for all colours } c
$$

Queries: “Is conjugate colour bar applied twice equal to identity for tensor species conjugation?” · “\overline{\overline{c}} = c conjugation involution tensor index colour” · “ConjTensorSpecies bar involutive conjugate colour twice”

### `ConjTensorSpecies.componentReindex`

def in `Physlib.Relativity.Tensors.Conjugation.Basic`

```lean
ConjTensorSpecies.componentReindex {k : Type} [CommRing k] [StarRing k] {C G : Type} [Group G] {V : C → Type}
  [(c : C) → AddCommGroup (V c)] [(c : C) → Module k (V c)] {basisIdx : C → Type} [(c : C) → Fintype (basisIdx c)]
  [(c : C) → DecidableEq (basisIdx c)] {rep : (c : C) → Representation k G (V c)}
  {b : (c : C) → Module.Basis (basisIdx c) k (V c)} (S : ConjTensorSpecies k C G V basisIdx rep b) {n : ℕ}
  (c : Fin n → C) : (TensorSpecies.Tensor.ComponentIdx fun i => S.bar (c i)) ≃ TensorSpecies.Tensor.ComponentIdx c
```

Docstring: Reindex component labels of `bar ∘ c` back to `c`, slotwise via the cast `barIdx_eq`.

**Reindexing of tensor component labels along conjugate colours**. For a conjugation structure $S$ on a tensor species and a list of index colours $c : \mathrm{Fin}\,n \to C$, this is an equivalence between the component indices of tensors with colours $\overline{c_i}$ (the conjugate colours $\mathrm{bar}\circ c$) and the component indices of tensors with colours $c$. It is obtained slotwise by the cast along the identity $\mathrm{bar}(\mathrm{bar}\,c_i)=c_i$ (`barIdx_eq`), so that it relabels components without changing their values.

Queries: “equivalence between component indices of conjugate colour tensor and original colour tensor” · “reindex tensor components bar of colours back to colours Lean ConjTensorSpecies” · “conjugate colour involution component index equivalence Lorentz tensor spinor”

### `Cosmology.SpatialGeometry`

inductive in `Physlib.Cosmology.FLRW.Basic`

```lean
Cosmology.SpatialGeometry : Type
```

Docstring: The inductive type with three constructors: - `Spherical (k : ℝ)` - `Flat` - `Saddle (k : ℝ)`

**Spatial geometry of an FLRW model**. An inductive type describing the constant-curvature spatial sections of an FLRW cosmological model, with three cases: spherical geometry with curvature parameter $k$ (closed universe), flat Euclidean geometry, and saddle (hyperbolic) geometry with curvature parameter $k$ (open universe).

Aliases: FLRW spatial sections; closed, flat and open spatial geometries

Queries: “what are the spatial geometries of an FLRW universe: spherical, flat, or saddle” · “inductive type closed flat open curvature k FLRW spatial sections” · “FLRW spatial curvature spherical flat saddle Lean type”

### `Electromagnetism.TempDistElectromagneticPotential.fieldStrength_eq_basis`

theorem in `Physlib.Electromagnetism.TemperedDistributional.FieldStrength`

```lean
Electromagnetism.TempDistElectromagneticPotential.fieldStrength_eq_basis {d : ℕ}
  (A : Electromagnetism.TempDistElectromagneticPotential d) (ε : SchwartzMap (SpaceTime d) ℝ) :
  (Electromagnetism.TempDistElectromagneticPotential.fieldStrength A) ε =
    ∑ μ,
      ∑ ν,
        (minkowskiMatrix μ μ * ((SpaceTime.distDeriv μ) A) ε ν -
            minkowskiMatrix ν ν * ((SpaceTime.distDeriv ν) A) ε μ) •
          Lorentz.Vector.basis μ ⊗ₜ[ℝ] Lorentz.Vector.basis ν
```

**Basis expansion of the distributional electromagnetic field strength**. For a tempered distributional electromagnetic potential $A$ and a Schwartz test function $\varepsilon$, the field strength tensor $F$ applied to $\varepsilon$ equals the sum over indices $\mu,\nu$ of $(\eta_{\mu\mu}\,(\partial_\mu A)(\varepsilon)_\nu-\eta_{\nu\nu}\,(\partial_\nu A)(\varepsilon)_\mu)$ times the basis tensor $e_\mu\otimes e_\nu$, where $\eta=$ `minkowskiMatrix` is the Minkowski metric and $e_\mu$ is `Lorentz.Vector.basis`. The distributional derivatives are `SpaceTime.distDeriv`, so this writes the field strength in components of the basis of contravariant Lorentz vectors.

$$
F(\varepsilon)=\sum_{\mu,\nu}\left(\eta_{\mu\mu}\,\partial_\mu A^{\nu}(\varepsilon)-\eta_{\nu\nu}\,\partial_\nu A^{\mu}(\varepsilon)\right)\, e_\mu\otimes e_\nu
$$

Queries: “how to write the electromagnetic field strength tensor of a distributional potential in a basis” · “\sum_{\mu,\nu} (\eta_{\mu\mu}\partial_\mu A_\nu - \eta_{\nu\nu}\partial_\nu A_\mu) e_\mu\otimes e_\nu” · “field strength distribution basis expansion minkowski derivative tensor”

### `EuclideanGroup.translation`

abbrev in `Physlib.SpaceAndTime.Space.EuclideanGroup.Basic`

```lean
EuclideanGroup.translation {n : ℕ} (self : EuclideanGroup n) : EuclideanSpace ℝ (Fin n)
```

Docstring: The translation part of a Euclidean transformation.

**Translation part of a Euclidean group element**. Projection of an element of the Euclidean group of dimension $n$ onto its translation part, a vector in $\mathbb{R}^n$. Elements of the Euclidean group are pairs consisting of a translation and an orthogonal linear map, and this function returns the translation component.

Queries: “translation component of a Euclidean group element in dimension n” · “which vector in R^n gives the translation part of an element of the Euclidean group” · “Euclidean group translation vector projection semidirect product orthogonal map”

### `Lorentz.Vector.actionCLM_surjective`

theorem in `Physlib.Relativity.Tensors.RealTensor.Vector.Tensorial`

```lean
Lorentz.Vector.actionCLM_surjective {d : ℕ} (Λ : ↑(LorentzGroup d)) : Function.Surjective ⇑(Lorentz.Vector.actionCLM Λ)
```

**Surjectivity of the Lorentz action on real Lorentz vectors**. For every Lorentz transformation $\Lambda$ in the Lorentz group $O(1,d)$, the continuous linear map `Lorentz.Vector.actionCLM` $\Lambda$, sending a real contravariant Lorentz vector $p$ to $\Lambda p$, is surjective on `Lorentz.Vector d`. This holds because $\Lambda$ is invertible with inverse given by the action of its group inverse.

$$
\forall \Lambda \in O(1,d),\ \Lambda \colon \mathbb{R}^{1+d} \to \mathbb{R}^{1+d} \text{ is surjective}
$$

Queries: “Is the action of a Lorentz transformation on Lorentz vectors surjective?” · “\Lambda \in O(1,d) \implies \Lambda \colon \mathbb{R}^{1+d} \to \mathbb{R}^{1+d} \text{ surjective}” · “Lorentz vector action surjective LorentzGroup actionCLM”

### `Lorentz.Vector.smul_add`

theorem in `Physlib.Relativity.Tensors.RealTensor.Vector.Tensorial`

```lean
Lorentz.Vector.smul_add {d : ℕ} (Λ : ↑(LorentzGroup d)) (p q : Lorentz.Vector d) : Λ • (p + q) = Λ • p + Λ • q
```

**Additivity of the Lorentz group action on Lorentz vectors**. For a Lorentz transformation $\Lambda$ in the Lorentz group $O(1,d)$ and real contravariant Lorentz vectors $p$ and $q$, applying $\Lambda$ to the sum $p + q$ gives the sum of the images $\Lambda \cdot p$ and $\Lambda \cdot q$. That is, the action of the Lorentz group on `Lorentz.Vector d` is additive.

$$
\Lambda \cdot (p + q) = \Lambda \cdot p + \Lambda \cdot q
$$

Queries: “Does the Lorentz group action distribute over addition of Lorentz vectors?” · “\Lambda \cdot (p + q) = \Lambda \cdot p + \Lambda \cdot q” · “Lorentz group action additive vector sum smul_add”

### `Lorentz.Vector.smul_eq_sum`

theorem in `Physlib.Relativity.Tensors.RealTensor.Vector.Tensorial`

```lean
Lorentz.Vector.smul_eq_sum {d : ℕ} (i : Fin 1 ⊕ Fin d) (Λ : ↑(LorentzGroup d)) (p : Lorentz.Vector d) :
  (Λ • p) i = ∑ j, ↑Λ i j * p j
```

**Components of a Lorentz group action on a vector**. For a Lorentz transformation $\Lambda$ in the Lorentz group $O(1,d)$ and a contravariant Lorentz vector $p$, the component of $\Lambda \cdot p$ at index $i \in$ `Fin 1 ⊕ Fin d` equals the sum over $j$ of the matrix entry $\Lambda_{ij}$ times the component $p_j$ of $p$. In other words, the Lorentz group acts on vectors by ordinary matrix-vector multiplication in the standard basis.

$$
(\Lambda \cdot p)^{i} = \sum_{j} \Lambda^{i}{}_{j}\, p^{j}
$$

Queries: “how does a Lorentz transformation act on components of a Lorentz vector as a matrix product” · “(Λ • p) i = ∑ j, Λ i j * p j Lorentz vector action” · “Lorentz group action on contravariant vector components matrix multiplication sum”

### `Lorentz.Vector.smul_neg`

theorem in `Physlib.Relativity.Tensors.RealTensor.Vector.Tensorial`

```lean
Lorentz.Vector.smul_neg {d : ℕ} (Λ : ↑(LorentzGroup d)) (p : Lorentz.Vector d) : Λ • -p = -(Λ • p)
```

**Lorentz group action commutes with negation on Lorentz vectors**. For every element $\Lambda$ of the Lorentz group $O(1,d)$ and every real contravariant Lorentz vector $p$, acting by $\Lambda$ on $-p$ gives the negative of acting by $\Lambda$ on $p$. This is linearity of the Lorentz group action on vectors with respect to negation.

$$
\Lambda \cdot (-p) = -(\Lambda \cdot p)
$$

Queries: “Does a Lorentz transformation send minus a four-vector to minus its transform?” · “\Lambda \cdot (-p) = -(\Lambda \cdot p) for Lorentz vectors” · “Lorentz group action negation linear vector smul_neg”

### `MSSMACC.AnomalyFreePerp.perpY₃`

theorem in `Physlib.Particles.SuperSymmetry.MSSMNu.AnomalyCancellation.OrthogY3B3.Basic`

```lean
MSSMACC.AnomalyFreePerp.perpY₃ (self : MSSMACC.AnomalyFreePerp) : (MSSMACC.dot MSSMACC.Y₃.val) self.val = 0
```

**Anomaly-free solutions orthogonal to Y3 are orthogonal to Y3**. For every element `x` of the type `MSSMACC.AnomalyFreePerp` of anomaly-free MSSM U(1) charge solutions orthogonal to $Y_3$ and $B_3$, the dot product of the charge vector $Y_3$ with the underlying charge of `x` is zero. This is the defining orthogonality condition packaged as a property of the subtype.

$$
\langle Y_3, x \rangle = 0
$$

Queries: “Why is the dot product of Y3 with an anomaly-free perpendicular MSSM charge zero?” · “\langle Y_3, x \rangle = 0 anomaly free MSSM charges” · “MSSM anomaly cancellation perpendicular Y3 B3 dot product zero”

### `Physlib.HTMLNote.content`

abbrev in `Physlib.Meta.Notes.HTMLNote`

```lean
Physlib.HTMLNote.content (self : Physlib.HTMLNote) : String
```

Docstring: The html contribution of the content.

**HTML content of a Physlib note**. Projection of a Physlib HTML note structure `Physlib.HTMLNote` onto its `content` field, the string of HTML extracted from an individual contribution (such as a command, informal command or note) to a Physlib note file. It is the accessor used when turning a declaration into an HTML note entry.

Queries: “How do I get the HTML string stored in a Physlib note contribution?” · “HTMLNote content field accessor string” · “Physlib note HTML content extraction from declaration”

### `Physlib.Wirtinger.dWirtingerCoord_block_sum`

theorem in `Physlib.Mathematics.Calculus.Wirtinger.Hessian`

```lean
Physlib.Wirtinger.dWirtingerCoord_block_sum {K : Type} [Fintype K] [DecidableEq K] {ι : K → Type}
  [(k : K) → Fintype (ι k)] [(k : K) → DecidableEq (ι k)] (f : (k : K) → (ι k → ℂ) → ℂ) (k₀ : K) (a : ι k₀)
  (u : (k : K) × ι k → ℂ) (hf : ∀ (k : K), DifferentiableAt ℝ (f k) (u ∘ Sigma.mk k)) :
  Physlib.Wirtinger.dWirtingerCoord (fun q => ∑ k, f k (q ∘ Sigma.mk k)) ⟨k₀, a⟩ u =
    Physlib.Wirtinger.dWirtingerCoord (f k₀) a (u ∘ Sigma.mk k₀)
```

Docstring: `∂_⟨k₀, a⟩` of a sum of sector functions is the `∂_a` of the sector-`k₀` function.

**Wirtinger derivative of a block sum of sector functions**. Let $f_k$ be real-differentiable functions on the coordinates of sector $k$, and let $u$ be a point of the total field space $\bigsqcup_k \iota_k \to \mathbb{C}$. The holomorphic Wirtinger derivative of the summed function $q \mapsto \sum_k f_k(q\circ\iota_k)$ with respect to the coordinate $\langle k_0, a\rangle$, evaluated at $u$, equals the Wirtinger derivative $\partial_a$ of the sector-$k_0$ function $f_{k_0}$ evaluated at the restriction $u\circ\iota_{k_0}$. The derivative of the other sectors vanishes, so only sector $k_0$ contributes. This is the statement that $\partial_{\langle k_0,a\rangle}$ of a sum of sector functions is $\partial_a$ of the sector-$k_0$ function, as used for block-diagonal Kähler metrics.

$$
\partial_{(k_0,a)}\Big(\sum_k f_k(\varphi_k)\Big)(u)=\partial_a f_{k_0}(u_{k_0})
$$

Queries: “Wirtinger derivative of a sum of functions on separate sectors equals derivative of the sector function” · “\partial_{(k_0,a)} \sum_k f_k(\varphi_k) = \partial_a f_{k_0}” · “Wirtinger derivative block sum sector Kähler potential coordinate”

### `Physlib.Wirtinger.hessianMatrixOf_add_eq_fromBlocks`

theorem in `Physlib.Mathematics.Calculus.Wirtinger.Hessian`

```lean
Physlib.Wirtinger.hessianMatrixOf_add_eq_fromBlocks {ι₁ : Type} [Fintype ι₁] [DecidableEq ι₁] {ι₂ : Type} [Fintype ι₂]
  [DecidableEq ι₂] (f₁ : (ι₁ → ℂ) → ℂ) (f₂ : (ι₂ → ℂ) → ℂ) (u : ι₁ ⊕ ι₂ → ℂ) (hf₁ : ContDiffAt ℝ 2 f₁ (u ∘ Sum.inl))
  (hf₂ : ContDiffAt ℝ 2 f₂ (u ∘ Sum.inr)) :
  Physlib.Wirtinger.hessianMatrixOf (fun q => f₁ (q ∘ Sum.inl) + f₂ (q ∘ Sum.inr)) u =
    Matrix.fromBlocks (Physlib.Wirtinger.hessianMatrixOf f₁ (u ∘ Sum.inl)) 0 0
      (Physlib.Wirtinger.hessianMatrixOf f₂ (u ∘ Sum.inr))
```

Docstring: The Hessian of `f₁ (q ∘ Sum.inl) + f₂ (q ∘ Sum.inr)` is `fromBlocks` of the two sector Hessians.

**Block-diagonal mixed Wirtinger Hessian of a sum of sector functions**. For functions $f_1$ and $f_2$ on complex coordinates indexed by $\iota_1$ and $\iota_2$, and a point $u$ on the disjoint union $\iota_1 \oplus \iota_2$, if $f_1$ and $f_2$ are $C^2$ at the restrictions of $u$ to the two summands, then the mixed Wirtinger Hessian of $q \mapsto f_1(q\circ\mathrm{inl}) + f_2(q\circ\mathrm{inr})$ at $u$ is the block-diagonal matrix with the Hessians of $f_1$ and $f_2$ at their respective sector points as diagonal blocks, and zero off-diagonal blocks (`Matrix.fromBlocks`). The mixed Hessian of a sum of functions in disjoint sectors has no cross terms.

$$
H\big(f_1(q\circ\mathrm{inl})+f_2(q\circ\mathrm{inr})\big)(u)=\begin{pmatrix}H(f_1)(u_1)&0\\0&H(f_2)(u_2)\end{pmatrix}
$$

Queries: “Hessian of a sum of functions on disjoint variable sets is block diagonal” · “\partial_I \bar\partial_J (f_1(\phi_1)+f_2(\phi_2)) = \text{block diagonal}” · “Wirtinger Hessian fromBlocks sector functions Kähler potential no mixing”

### `QuantumMechanics.OneDimension.HilbertSpace.MemHS`

def in `Physlib.QuantumMechanics.HilbertSpaces.OneDimension.Basic`

```lean
QuantumMechanics.OneDimension.HilbertSpace.MemHS (f : ℝ → ℂ) : Prop
```

Docstring: The proposition `MemHS f` for a function `f : ℝ → ℂ` is defined to be true if the function `f` can be lifted to the Hilbert space.

**Membership predicate for the one-dimensional quantum Hilbert space L²(ℝ, ℂ)**. The predicate `MemHS f` holds for a function $f : \mathbb{R} \to \mathbb{C}$ when $f$ is square-integrable with respect to Lebesgue measure, i.e. $f$ lies in $L^2(\mathbb{R}, \mathbb{C})$, so that it defines a state of the one-dimensional quantum system. It is the condition that $f$ can be lifted to the Hilbert space `QuantumMechanics.OneDimension.HilbertSpace`.

$$
f \in L^2(\mathbb{R}, \mathbb{C})
$$

Queries: “when is a function ℝ → ℂ a valid wave function in one-dimensional quantum mechanics” · “square integrable function on the real line with complex values Lebesgue measure membership” · “MemHS f L2 Hilbert space one dimension quantum state condition”

### `QuantumMechanics.OneDimension.HilbertSpace.ext_iff`

theorem in `Physlib.QuantumMechanics.HilbertSpaces.OneDimension.Basic`

```lean
QuantumMechanics.OneDimension.HilbertSpace.ext_iff {f g : ↥QuantumMechanics.OneDimension.HilbertSpace} :
  f = g ↔ ↑↑f =ᵐ[MeasureTheory.volume] ↑↑g
```

**Extensionality for one-dimensional quantum wave functions**. Two elements $f$ and $g$ of the one-dimensional quantum mechanics Hilbert space $L^2(\mathbb{R}, \mathbb{C})$ are equal exactly when their representative functions agree almost everywhere with respect to Lebesgue measure (`MeasureTheory.volume`). So a state is determined by its wave function up to a null set.

$$
f = g \iff f =_{\text{a.e.}} g
$$

Queries: “when are two wave functions in L^2(R, C) equal almost everywhere Lebesgue measure” · “f = g iff f equals g almost everywhere quantum Hilbert space one dimension” · “extensionality L2 wave function ae equal Lebesgue volume quantum state”

### `QuantumMechanics.OneDimension.HilbertSpace.mk_eq_iff`

theorem in `Physlib.QuantumMechanics.HilbertSpaces.OneDimension.Basic`

```lean
QuantumMechanics.OneDimension.HilbertSpace.mk_eq_iff {f g : ℝ → ℂ}
  {hf : QuantumMechanics.OneDimension.HilbertSpace.MemHS f} {hg : QuantumMechanics.OneDimension.HilbertSpace.MemHS g} :
  QuantumMechanics.OneDimension.HilbertSpace.mk hf = QuantumMechanics.OneDimension.HilbertSpace.mk hg ↔
    f =ᵐ[MeasureTheory.volume] g
```

**Equality of one-dimensional quantum states up to almost-everywhere equality**. Two square-integrable wave functions $f, g : \mathbb{R} \to \mathbb{C}$ (given by proofs `hf` and `hg` of `MemHS`) define the same element of the one-dimensional Hilbert space $L^2(\mathbb{R}, \mathbb{C})$ via `QuantumMechanics.OneDimension.HilbertSpace.mk` exactly when $f$ and $g$ agree Lebesgue-almost everywhere. So states are identified up to null sets.

$$
[f]=[g] \iff f=g \text{ almost everywhere}
$$

Queries: “when do two wave functions give the same state in L^2(R, C) equal almost everywhere” · “\[ \mathrm{mk}(f)=\mathrm{mk}(g) \iff f=g \text{ a.e.} \]” · “one-dimensional quantum Hilbert space mk equality almost everywhere Lebesgue measure”

### `RigidBody.rotationalKineticEnergy_eq_integral`

theorem in `Physlib.ClassicalMechanics.RigidBody.KineticEnergy`

```lean
RigidBody.rotationalKineticEnergy_eq_integral (R : RigidBody 3) (ω : Fin 3 → ℝ) :
  R.rotationalKineticEnergy ω = 1 / 2 * R.ρ ⟨fun x => (crossProduct ω) (x.val 3) ⬝ᵥ (crossProduct ω) (x.val 3), ⋯⟩
```

Docstring: The rotational kinetic energy equals the mass integral of the local rotational speed squared: `T = ½ ∫ |ω × r|² dm`.

**Rotational kinetic energy as a mass integral**. For a rigid body $R$ in three dimensions and angular velocity $\omega \in \mathbb{R}^3$, the rotational kinetic energy `RigidBody.rotationalKineticEnergy` equals one half of the mass integral of the squared norm of $\omega \times r$, where $r$ is the position of a body point and the integral is taken against the mass distribution `RigidBody.ρ`. The statement is given in the Lean form with the squared cross product $(\omega \times x_3)\cdot(\omega \times x_3)$ as the integrand.

$$
T = \frac{1}{2}\int |\omega \times r|^2 \, dm
$$

Queries: “rotational kinetic energy of a rigid body equals half the integral of |ω × r|² dm” · “\frac{1}{2}\int |\omega \times r|^2\,dm” · “rigid body rotation kinetic energy mass integral cross product angular velocity”

### `SULieAlgebra.adjoint_val`

theorem in `Physlib.Mathematics.Groups.SpecialUnitary.LieAlgebra.Adjoint`

```lean
SULieAlgebra.adjoint_val {n : ℕ} (g : ↥(Matrix.specialUnitaryGroup (Fin n) ℂ)) (x : SULieAlgebra n ℂ) :
  ↑((SULieAlgebra.adjoint g) x) = ↑g * ↑x * star ↑g
```

Docstring: The underlying matrix of `adjoint g x` is `g x g†`.

**Adjoint action of SU(n) on its Lie algebra in matrix form**. For $g$ in the special unitary group $SU(n)$ and $x$ in the Lie algebra $\mathfrak{su}(n)$, the underlying matrix of the adjoint action `SULieAlgebra.adjoint g x` is the matrix product $g x g^\dagger$, where $g^\dagger$ is the conjugate transpose of $g$ (written `star`).

$$
\operatorname{Ad}_g(x) = g\,x\,g^{\dagger}
$$

Queries: “what is the underlying matrix of the adjoint action of SU(n) on su(n)? g x g^dagger” · “\operatorname{Ad}_g(x) = g x g^{\dagger} for g in SU(n), x in su(n)” · “adjoint representation SU(n) conjugation matrix coercion star special unitary Lie algebra”

### `SULieAlgebra.adjointℂ_tmul`

theorem in `Physlib.Mathematics.Groups.SpecialUnitary.LieAlgebra.Adjoint`

```lean
SULieAlgebra.adjointℂ_tmul {n : ℕ} (g : ↥(Matrix.specialUnitaryGroup (Fin n) ℂ)) (z : ℂ) (x : SULieAlgebra n ℂ) :
  (SULieAlgebra.adjointℂ g) (z ⊗ₜ[ℝ] x) = z ⊗ₜ[ℝ] (SULieAlgebra.adjoint g) x
```

**Complexified adjoint action on elementary tensors**. For $g$ in $SU(n)$, the complexified adjoint representation `SULieAlgebra.adjointℂ` sends an elementary tensor $z \otimes x$ with $z \in \mathbb{C}$ and $x \in \mathfrak{su}(n)$ to $z \otimes \mathrm{Ad}_g(x)$, where `SULieAlgebra.adjoint` $g$ acts on $x$ by conjugation $x \mapsto g x g^\dagger$. So the complex scalar passes through the action unchanged and only the real Lie algebra element is conjugated.

$$
\mathrm{Ad}^{\mathbb{C}}_g(z \otimes x) = z \otimes \mathrm{Ad}_g(x)
$$

Queries: “adjoint representation of SU(n) on complexification applied to z tensor x equals z tensor adjoint of x” · “\mathrm{Ad}_g(z \otimes x) = z \otimes g x g^\dagger” · “SU(n) adjoint action complexified tensor product elementary tensor lemma su(n)”

### `SULieAlgebra.toMatrixℂ_adjointℂ_apply_of_diagonal`

theorem in `Physlib.Mathematics.Groups.SpecialUnitary.LieAlgebra.Adjoint`

```lean
SULieAlgebra.toMatrixℂ_adjointℂ_apply_of_diagonal {n : ℕ} {g : ↥(Matrix.specialUnitaryGroup (Fin n) ℂ)} {d : Fin n → ℂ}
  (hg : ↑g = Matrix.diagonal d) (A : SULieAlgebra.Complexification n) (x y : Fin n) :
  SULieAlgebra.toMatrixℂ ((SULieAlgebra.adjointℂ g) A) x y = d x * star (d y) * SULieAlgebra.toMatrixℂ A x y
```

Docstring: The adjoint action of a diagonal element of `SU(n)` scales the entry at `(x, y)` of the matrix by `d x * star (d y)`.

**Adjoint action of a diagonal special unitary element on matrix entries**. Let $g \in SU(n)$ be a diagonal matrix $g = \operatorname{diag}(d)$ and let $A$ be an element of the complexified Lie algebra $\mathfrak{su}(n)\otimes\mathbb{C}$. Then the complexified adjoint action $A \mapsto g A g^\dagger$ (`SULieAlgebra.adjointℂ`) multiplies the $(x,y)$ entry of $A$ by $d_x \overline{d_y}$, where $d_x$ and $d_y$ are the diagonal entries of $g$.

$$
(\operatorname{Ad}_g A)_{xy} = d_x\,\overline{d_y}\,A_{xy}
$$

Queries: “How does the adjoint action of a diagonal SU(n) element scale matrix entries?” · “$(gAg^\dagger)_{xy} = d_x \overline{d_y} A_{xy}$ for diagonal g in SU(n)” · “adjoint representation SU(n) diagonal matrix entry scaling star complexification”

### `Space.fderiv_fun_slice_symm_right_apply`

theorem in `Physlib.SpaceAndTime.Space.Slice`

```lean
Space.fderiv_fun_slice_symm_right_apply {F : Type} [NormedAddCommGroup F] [NormedSpace ℝ F] {d : ℕ} (i : Fin d.succ)
  (r : ℝ) (x1 x2 : Space d) (f : Space d.succ → F) (hf : DifferentiableAt ℝ f ((Space.slice i).symm (r, x1))) :
  (fderiv ℝ (fun x => f ((Space.slice i).symm (r, x))) x1) x2 =
    (fderiv ℝ f ((Space.slice i).symm (r, x1))) ((Space.slice i).symm (0, x2))
```

**Partial derivative in a coordinate slice equals the derivative along the complementary directions**. Let $f$ be a function from $\mathrm{Space}(d+1)$ to a real normed space $F$, differentiable at the point $\iota_i(r,x_1)$, where $\iota_i$ is the inverse of the slicing equivalence `Space.slice` that inserts the value $r$ in the $i$-th coordinate. Then the derivative in $x_1$ of the map $x \mapsto f(\iota_i(r,x))$ at $x_1$, applied to a vector $x_2$, equals the derivative of $f$ at $\iota_i(r,x_1)$ applied to $\iota_i(0,x_2)$, that is, the directional derivative of $f$ along the vector obtained from $x_2$ with zero $i$-th component.

$$
D_{x_1}\big(f(\iota_i(r,x_1))\big)\cdot x_2 = Df(\iota_i(r,x_1))\big(\iota_i(0,x_2)\big)
$$

Queries: “derivative of f restricted to a coordinate slice equals fderiv of f in the spatial directions with zero time component” · “fderiv of x ↦ f(slice i symm (r, x)) applied to x2 equals fderiv f applied to slice i symm (0, x2)” · “partial derivative along slice coordinate Space d succ fderiv slice symm chain rule”

### `Space.norm_left_le_norm_slice_symm`

theorem in `Physlib.SpaceAndTime.Space.Slice`

```lean
Space.norm_left_le_norm_slice_symm {d : ℕ} (i : Fin d.succ) (r : ℝ) (x : Space d) : ‖x‖ ≤ ‖(Space.slice i).symm (r, x)‖
```

**Norm of a point is bounded by the norm of its slice embedding**. For any index $i$ in $\mathrm{Fin}(d+1)$, any real $r$ and any spatial point $x$ in `Space d`, the Euclidean norm of $x$ is at most the norm of the point in `Space d.succ` obtained by inserting $r$ as the $i$-th coordinate of $x$, that is, the image of $(r, x)$ under the inverse of `Space.slice`. The statement holds because the inserted coordinate can only add to the squared norm.

$$
\|x\| \le \left\|\mathrm{slice}_i^{-1}(r, x)\right\|
$$

Queries: “norm of a point in space is at most norm after inserting a coordinate slice” · “\|x\| \le \|\mathrm{slice}_i^{-1}(r,x)\| Space slice norm inequality” · “Space slice symm norm bound coordinate insertion Euclidean”

### `Space.slice_symm_apply`

theorem in `Physlib.SpaceAndTime.Space.Slice`

```lean
Space.slice_symm_apply {d : ℕ} (i : Fin d.succ) (r : ℝ) (x : Space d) :
  ((Space.slice i).symm (r, x)).val = fun j => (Fin.insertNthEquiv (fun x => ℝ) i) (r, x.val) j
```

**Inverse of the coordinate slice map on space**. For a point $(r, x)$ in $\mathbb{R} \times \text{Space}(d)$, the underlying coordinate function of its preimage under the slice equivalence `Space.slice` $i$ is obtained by inserting $r$ as the $i$-th coordinate into the coordinates of $x$, using `Fin.insertNthEquiv`. So the $i$-th coordinate of the result is $r$ and the remaining coordinates are those of $x$.

$$
\operatorname{slice}_i^{-1}(r, x)_j = \operatorname{insertNth}_i(r, x)_j
$$

Queries: “what are the coordinates of the inverse slice map on space Space d.succ” · “\text{slice}^{-1}(r,x)_j = \text{insertNth}_i(r,x)_j” · “Space slice symm apply insertNth coordinate equivalence”

### `Space.timeIntegralSchwartz`

def in `Physlib.SpaceAndTime.TimeAndSpace.ConstantTimeDist`

```lean
Space.timeIntegralSchwartz {d : ℕ} : SchwartzMap (Time × Space d) ℝ →L[ℝ] SchwartzMap (Space d) ℝ
```

Docstring: The continuous linear map taking Schwartz maps on `Time × Space d` to `space d` by integrating over time.

**Time integral of Schwartz maps as a continuous linear map**. This is the continuous linear map $\mathcal{S}(\mathbb{R}_t \times \mathbb{R}^d, \mathbb{R}) \to \mathcal{S}(\mathbb{R}^d, \mathbb{R})$ that sends a Schwartz function $f$ on time times space to the function $x \mapsto \int_{t} f(t, x)\,dt$ on space, obtained by integrating over the time coordinate. It is used to pass from distributions on spacetime to distributions on space that are constant in time.

Queries: “integrate Schwartz function over time to get Schwartz function on space” · “continuous linear map from Schwartz space on time times space to Schwartz space on space” · “\int_{\mathbb{R}} f(t,x)\,dt \in \mathcal{S}(\mathbb{R}^d)” · “Schwartz map time integral space d constant in time distribution”

### `SuperSymmetry.SU5.ChargeSpectrum.allowsTermForm_allowsTerm`

theorem in `Physlib.Particles.SuperSymmetry.SU5.ChargeSpectrum.AllowsTerm`

```lean
SuperSymmetry.SU5.ChargeSpectrum.allowsTermForm_allowsTerm {𝓩 : Type} [AddCommGroup 𝓩] [DecidableEq 𝓩] {a b c : 𝓩}
  {T : SuperSymmetry.SU5.PotentialTerm} : (SuperSymmetry.SU5.ChargeSpectrum.allowsTermForm a b c T).AllowsTerm T
```

Docstring: The charge spectrum `allowsTermForm a b c T` allows the potential term `T`.

**Charge spectrum from three charges allows its potential term**. For any charges $a$, $b$, $c$ in an additive commutative group $\mathcal{Z}$ and any SU(5) supersymmetric potential term $T$, the charge spectrum `SuperSymmetry.SU5.ChargeSpectrum.allowsTermForm a b c T` allows the term $T$, meaning that zero is among the total charges of instances of $T$ under this charge spectrum. This is the guarantee built into the construction of `allowsTermForm`.

Queries: “does the charge spectrum built from three charges always allow the SU(5) potential term” · “allowsTermForm a b c T AllowsTerm T SU(5) supersymmetric GUT” · “SU5 charge spectrum allows potential term zero total charge superpotential keywords”

### `SuperSymmetry.SU5.ChargeSpectrum.allowsTerm_of_eq_allowsTermForm`

theorem in `Physlib.Particles.SuperSymmetry.SU5.ChargeSpectrum.AllowsTerm`

```lean
SuperSymmetry.SU5.ChargeSpectrum.allowsTerm_of_eq_allowsTermForm {𝓩 : Type} [AddCommGroup 𝓩] [DecidableEq 𝓩]
  {T : SuperSymmetry.SU5.PotentialTerm} (x : SuperSymmetry.SU5.ChargeSpectrum 𝓩)
  (h : ∃ a b c, x = SuperSymmetry.SU5.ChargeSpectrum.allowsTermForm a b c T) : x.AllowsTerm T
```

**Charge spectrum of the form allowsTermForm allows the term**. If a charge spectrum $x$ of the SU(5) supersymmetric GUT equals `ChargeSpectrum.allowsTermForm a b c T` for some charges $a, b, c$ in an additive commutative group $\mathcal{Z}$, then $x$ allows the potential term $T$, meaning zero is among the total charges of instances of $T$. This is the converse direction of the characterisation of allowed terms by `allowsTermForm`.

Queries: “SU(5) charge spectrum of the form allowsTermForm allows potential term” · “if a charge spectrum equals allowsTermForm a b c then it allows the term T” · “supersymmetric GUT U(1) charges zero total charge allowed superpotential term”

### `SuperSymmetry.SU5.ChargeSpectrum.minimallyAllowsTermOfFinset_subset_of_subset`

theorem in `Physlib.Particles.SuperSymmetry.SU5.ChargeSpectrum.MinimallyAllowsTerm.OfFinset`

```lean
SuperSymmetry.SU5.ChargeSpectrum.minimallyAllowsTermOfFinset_subset_of_subset {𝓩 : Type} [DecidableEq 𝓩]
  [AddCommGroup 𝓩] {S5 S5' S10 S10' : Finset 𝓩} {T : SuperSymmetry.SU5.PotentialTerm} (h5 : S5' ⊆ S5)
  (h10 : S10' ⊆ S10) :
  SuperSymmetry.SU5.ChargeSpectrum.minimallyAllowsTermsOfFinset S5' S10' T ⊆
    SuperSymmetry.SU5.ChargeSpectrum.minimallyAllowsTermsOfFinset S5 S10 T
```

**Monotonicity of minimally allowed charge spectra in the charge sets**. If $S_5'$ is a subset of $S_5$ and $S_{10}'$ is a subset of $S_{10}$, then every charge spectrum in the multiset of minimally allowed charge spectra for the potential term $T$ built from $S_5'$ and $S_{10}'$ also appears in the corresponding multiset built from $S_5$ and $S_{10}$. This is the monotonicity of `minimallyAllowsTermsOfFinset` in its two finite charge sets, for a fixed term $T$ in the SU(5) supersymmetric GUT.

$$
S_5' \subseteq S_5 \wedge S_{10}' \subseteq S_{10} \implies \mathrm{minimallyAllowsTermsOfFinset}(S_5', S_{10}', T) \subseteq \mathrm{minimallyAllowsTermsOfFinset}(S_5, S_{10}, T)
$$

Queries: “minimally allowed charge spectra subset monotone in charge sets SU(5) SUSY GUT” · “\mathrm{minimallyAllowsTermsOfFinset}(S_5', S_{10}', T) \subseteq \mathrm{minimallyAllowsTermsOfFinset}(S_5, S_{10}, T)” · “SU5 charge spectrum potential term minimal allowed subset lemma”

### `SuperSymmetry.SU5.ChargeSpectrum.minimallyAllowsTermsOfFinset`

def in `Physlib.Particles.SuperSymmetry.SU5.ChargeSpectrum.MinimallyAllowsTerm.OfFinset`

```lean
SuperSymmetry.SU5.ChargeSpectrum.minimallyAllowsTermsOfFinset {𝓩 : Type} [DecidableEq 𝓩] [AddCommGroup 𝓩]
  (S5 S10 : Finset 𝓩) (T : SuperSymmetry.SU5.PotentialTerm) : Multiset (SuperSymmetry.SU5.ChargeSpectrum 𝓩)
```

Docstring: The multiset of all charges within `ofFinset S5 S10` which minimally allow the potential term `T`.

**Multiset of minimally allowed charge spectra from finite charge sets**. This defines, for finite sets $S_5$ and $S_{10}$ of charges in $\mathcal{Z}$ and a potential term $T$ of the $SU(5)$ supersymmetric GUT, the multiset of all charge spectra with charges drawn from $S_5$ and $S_{10}$ that minimally allow $T$, meaning they allow $T$ and no strict subset of their charge content allows it. The multiset is built from finite sets so that duplicates need not be removed by hand.

Queries: “SU(5) SUSY GUT charge spectra minimally allowing a superpotential term from finite charge sets” · “which U(1) charge assignments minimally allow the W2 or W1 potential term in SU(5) supersymmetry” · “minimally allowed charge spectrum multiset Finset S5 S10 potential term SU(5) grand unified theory”

### `WickContraction.consAddContract_getDual?_zero`

theorem in `Physlib.QFT.PerturbationTheory.WickContraction.Card`

```lean
WickContraction.consAddContract_getDual?_zero {n : ℕ} (i : Fin n.succ) (c : WickContraction n) :
  (WickContraction.consAddContract i c).getDual? 0 = some i.succ
```

**Dual of the first field under consAddContract**. For a Wick contraction $c$ of $n$ fields and an index $i$ in $\text{Fin}(n+1)$, the contraction `WickContraction.consAddContract` $i$ $c$ of $n+2$ fields pairs the new field at position $0$ with the field at position $i+1$. Hence `WickContraction.getDual?` applied to $0$ returns `some i.succ`, the index $i+1$ in $\text{Fin}(n+2)$.

$$
\mathrm{getDual?}_{\mathrm{cons\text{-}add}(i,c)}(0) = \mathrm{some}(i+1)
$$

Queries: “Which field is contracted with field 0 after consAddContract inserts a pair of fields?” · “getDual? 0 (consAddContract i c) = some i.succ Wick contraction” · “Wick contraction inserted pair contracted position zero dual index succ”

### `WickContraction.exists_join_singleton_of_card_ge_zero`

theorem in `Physlib.QFT.PerturbationTheory.WickContraction.Join`

```lean
WickContraction.exists_join_singleton_of_card_ge_zero {𝓕 : FieldSpecification} {φs : List 𝓕.FieldOp}
  (φsΛ : WickContraction φs.length) (h : 0 < (↑φsΛ).card) (hc : WickContraction.GradingCompliant φs φsΛ) :
  ∃ i j,
    ∃ (h : i < j),
      ∃ φsucΛ,
        φsΛ = (WickContraction.singleton h).join φsucΛ ∧
          𝓕.fieldOpStatistic φs[i] = 𝓕.fieldOpStatistic φs[j] ∧
            WickContraction.GradingCompliant (WickContraction.singleton h).uncontractedListGet φsucΛ ∧
              (↑φsucΛ).card + 1 = (↑φsΛ).card
```

**Existence of a removable grading-compliant pair in a Wick contraction**. If a grading-compliant Wick contraction $\varphi s\Lambda$ of a list of field operators has at least one contracted pair, then there exist indices $i<j$ such that the single pair $(i,j)$ can be split off: $\varphi s\Lambda$ equals the join of the singleton contraction at $(i,j)$ with some contraction $\varphi s\Lambda^{uc}$ of the uncontracted list. Moreover the fields at $i$ and $j$ have the same statistic, the remaining contraction is grading compliant, and it has one fewer pair than $\varphi s\Lambda$.

$$
\exists\, i<j,\ \exists\,\varphi s\Lambda^{uc},\ \varphi s\Lambda = \{(i,j)\} \cdot \varphi s\Lambda^{uc},\ \text{stat}(\varphi s_i)=\text{stat}(\varphi s_j),\ |\varphi s\Lambda^{uc}|+1=|\varphi s\Lambda|
$$

Queries: “Wick contraction with positive number of pairs can split off a singleton pair with matching statistics” · “\varphi s\Lambda = \{(i,j)\} \cdot \varphi s\Lambda^{uc} grading compliant Wick contraction induction” · “Wick contraction join singleton grading compliant fermionic bosonic statistic card”

### `WickContraction.joinLiftRight_injective`

theorem in `Physlib.QFT.PerturbationTheory.WickContraction.Join`

```lean
WickContraction.joinLiftRight_injective {𝓕 : FieldSpecification} {φs : List 𝓕.FieldOp} {φsΛ : WickContraction φs.length}
  {φsucΛ : WickContraction φsΛ.uncontractedListGet.length} : Function.Injective WickContraction.joinLiftRight
```

**Injectivity of the lift of uncontracted pairs into a joined Wick contraction**. The map `WickContraction.joinLiftRight` sending a Wick contraction of the uncontracted list of a Wick contraction $\varphi s\Lambda$ to the corresponding joined Wick contraction of $\varphi s$ is injective. Its arguments are the list of field operators $\varphi s$, a Wick contraction $\varphi s\Lambda$ of it, and a Wick contraction of the uncontracted list $[\varphi s\Lambda]^{uc}$.

Queries: “Is the lift of contractions of the uncontracted part into the joined Wick contraction injective?” · “joinLiftRight injective Wick contraction join uncontracted list” · “injective map from subcontraction pairs to joined Wick contraction Physlib”

### `WickContraction.join_congr`

theorem in `Physlib.QFT.PerturbationTheory.WickContraction.Join`

```lean
WickContraction.join_congr {𝓕 : FieldSpecification} {φs : List 𝓕.FieldOp} {φsΛ : WickContraction φs.length}
  {φsucΛ : WickContraction φsΛ.uncontractedListGet.length} {φsΛ' : WickContraction φs.length} (h1 : φsΛ = φsΛ') :
  φsΛ.join φsucΛ = φsΛ'.join ((WickContraction.congr ⋯) φsucΛ)
```

**Congruence of joining Wick contractions**. If two Wick contractions $\varphi s\Lambda$ and $\varphi s\Lambda'$ of the same list of field operators are equal, then joining $\varphi s\Lambda$ with a contraction $\varphi s\Lambda^{uc}$ of its uncontracted list gives the same result as joining $\varphi s\Lambda'$ with the transported contraction `WickContraction.congr` applied to $\varphi s\Lambda^{uc}$. The lemma is a congruence statement that transports the second argument along the equality of the first.

$$
\varphi s\Lambda.\mathrm{join}\,\varphi s^{uc}\Lambda = \varphi s\Lambda'.\mathrm{join}\,(\mathrm{congr}\,\varphi s^{uc}\Lambda) \text{ if } \varphi s\Lambda = \varphi s\Lambda'
$$

Queries: “Wick contraction join congruence when the first contraction is equal” · “\varphi s\Lambda.\mathrm{join}\,\varphi s^{uc}\Lambda = \varphi s\Lambda'.\mathrm{join}\,(\mathrm{congr}\,\varphi s^{uc}\Lambda)” · “WickContraction join congr equality uncontracted list FieldOp”

### `WickContraction.join_fstFieldOfContract_joinLiftLeft`

theorem in `Physlib.QFT.PerturbationTheory.WickContraction.Join`

```lean
WickContraction.join_fstFieldOfContract_joinLiftLeft {𝓕 : FieldSpecification} {φs : List 𝓕.FieldOp}
  (φsΛ : WickContraction φs.length) (φsucΛ : WickContraction φsΛ.uncontractedListGet.length) (a : ↥↑φsΛ) :
  (φsΛ.join φsucΛ).fstFieldOfContract (WickContraction.joinLiftLeft a) = φsΛ.fstFieldOfContract a
```

**First position of a contracted pair is preserved under joining left-lifted contractions**. For a Wick contraction $\varphi s\Lambda$ of a list of field operators and a Wick contraction of its uncontracted list, joining the two and then taking the smaller position of a contracted pair gives the same result as taking it in $\varphi s\Lambda$, after lifting that pair via `WickContraction.joinLiftLeft`. In other words, `WickContraction.joinLiftLeft` preserves the first field position of each contracted pair from the left subcontraction `φsΛ`.

$$
\mathrm{fstFieldOfContract}_{\varphi s\Lambda \cup \varphi s^{uc}\Lambda}(\mathrm{joinLiftLeft}(a)) = \mathrm{fstFieldOfContract}_{\varphi s\Lambda}(a)
$$

Queries: “Does joining Wick contractions preserve the first position of contracted pairs from the left subcontraction?” · “\mathrm{fstFieldOfContract}(\mathrm{joinLiftLeft}(a)) = \mathrm{fstFieldOfContract}(a)” · “Wick contraction join fstFieldOfContract joinLiftLeft lemma”

### `WickContraction.mem_consAddContract_of_mem_iff`

theorem in `Physlib.QFT.PerturbationTheory.WickContraction.Card`

```lean
WickContraction.mem_consAddContract_of_mem_iff {n : ℕ} (i : Fin n.succ) (c : WickContraction n) (a : Finset (Fin n)) :
  a ∈ ↑c ↔ Finset.map (Fin.succEmb n.succ) (Finset.map i.succAboveEmb a) ∈ ↑(WickContraction.consAddContract i c)
```

**Membership of a contraction's pairs under consAddContract**. For a Wick contraction $c$ of $n$ fields, an index $i$ in $\mathrm{Fin}(n+1)$, and a finite set $a$ of indices in $\mathrm{Fin}(n)$, $a$ is a contracted pair of $c$ exactly when its image under the successor embedding of $\mathrm{Fin}(n+1)$ composed with the `Fin.succAboveEmb` insertion at $i$ is a contracted pair of `WickContraction.consAddContract` applied to $i$ and $c$. This is the membership lemma used to count Wick contractions by relating those of $n+2$ fields to those of $n$ fields.

$$
a \in c \iff \sigma(\iota_i(a)) \in \mathrm{consAddContract}(i, c)
$$

Queries: “Wick contraction pair membership preserved under consAddContract” · “\text{contraction } a \in c \iff \text{shifted } a \in \mathrm{consAddContract}(i,c)” · “Wick contraction cardinality consAddContract mem iff succAboveEmb Finset.map”

### `WithDim.val`

abbrev in `Physlib.Units.WithDim.Basic`

```lean
WithDim.val {B : Type} [DimensionBasis B] {d : Dimension B} {M : Type} (self : WithDim d M) : M
```

Docstring: The underlying value of `M`.

**Underlying value of a dimensioned quantity**. The projection `WithDim.val` extracts the underlying numerical value of type `M` from an element of `WithDim d M`, where `d` is a physical dimension over a basis `B` of base dimensions. It forgets the dimension tag, so the result is a plain element of `M`.

Queries: “how to get the numerical value out of a quantity with physical dimension in Physlib” · “WithDim underlying value projection M from dimensioned quantity” · “dimension tag strip value coercion unit type keywords WithDim val”

### `WithDim.withDim_hMul_val`

theorem in `Physlib.Units.WithDim.Basic`

```lean
WithDim.withDim_hMul_val {B : Type} [DimensionBasis B] {d1 d2 : Dimension B} (m1 : WithDim d1 ℝ) (m2 : WithDim d2 ℝ) :
  (m1 * m2).val = m1.val * m2.val
```

**Underlying value of a product of dimensioned quantities**. For real-valued quantities $m_1$ and $m_2$ carrying dimensions $d_1$ and $d_2$ (elements of `WithDim d1 ℝ` and `WithDim d2 ℝ`), the numerical value of their product `m1 * m2` is the product of their numerical values. The dimension tags are forgotten by `WithDim.val`, so this only concerns the underlying reals.

$$
(m_1 m_2).\mathrm{val} = m_1.\mathrm{val}\, m_2.\mathrm{val}
$$

Queries: “value of product of dimensioned quantities equals product of values withDim” · “(m1 * m2).val = m1.val * m2.val” · “WithDim val multiplication dimension units real numbers”

### `realLorentzTensor.leviCivita_antisymm`

theorem in `Physlib.Relativity.Tensors.LeviCivita.Basic`

```lean
realLorentzTensor.leviCivita_antisymm :
  realLorentzTensor.leviCivita = (TensorSpecies.Tensor.permT ![1, 0, 2, 3] ⋯) (-realLorentzTensor.leviCivita)
```

Docstring: The Levi-Civita tensor is antisymmetric in its first two indices `{ε4 | μ ν ρ σ = - ε4 | ν μ ρ σ}ᵀ`.

**Antisymmetry of the Levi-Civita tensor in its first two indices**. The rank-four Levi-Civita tensor $\varepsilon$ of real Lorentz tensors (with all indices contravariant, in dimension $d=3$, normalised by $\varepsilon^{0123}=1$) equals, after swapping its first two index slots, its own negative: $\varepsilon^{\mu\nu\rho\sigma} = -\varepsilon^{\nu\mu\rho\sigma}$. This is stated as an equality of tensors via the reindexing `TensorSpecies.Tensor.permT` with the permutation ![1, 0, 2, 3].

$$
\varepsilon^{\mu\nu\rho\sigma} = -\varepsilon^{\nu\mu\rho\sigma}
$$

Queries: “Is the Levi-Civita tensor antisymmetric in its first two indices in Lorentz tensors?” · “\varepsilon^{\mu\nu\rho\sigma} = -\varepsilon^{\nu\mu\rho\sigma}” · “Levi-Civita tensor swap first two indices antisymmetry permT realLorentzTensor”

### `realLorentzTensor.leviCivita_basis_repr_eq_leviCivitaSymbol`

theorem in `Physlib.Relativity.Tensors.LeviCivita.Basic`

```lean
realLorentzTensor.leviCivita_basis_repr_eq_leviCivitaSymbol
  (b :
    TensorSpecies.Tensor.ComponentIdx
      ![realLorentzTensor.Color.up, realLorentzTensor.Color.up, realLorentzTensor.Color.up,
        realLorentzTensor.Color.up]) :
  ((TensorSpecies.Tensor.basis
            ![realLorentzTensor.Color.up, realLorentzTensor.Color.up, realLorentzTensor.Color.up,
              realLorentzTensor.Color.up]).repr
        realLorentzTensor.leviCivita)
      b =
    ↑(leviCivitaSymbol fun i => finSumFinEquiv (b i))
```

Docstring: The components of the Levi-Civita tensor in the standard basis are the general-dimension Levi-Civita symbol `leviCivitaSymbol` of the multi-index at `ι = Fin 4`.

**Standard-basis components of the rank-four Levi-Civita tensor**. In the standard basis of contravariant Lorentz vectors, the component of the Levi-Civita tensor `realLorentzTensor.leviCivita` (four upper indices, dimension $d=3$) at an index tuple $b$ equals the general Levi-Civita symbol `leviCivitaSymbol` on `Fin 4` applied to the tuple $b$, after identifying the index set $\mathrm{Fin}\,1 \oplus \mathrm{Fin}\,3$ with $\mathrm{Fin}\,4$ via `finSumFinEquiv`. The value is the sign of the tuple when it is a permutation of the four indices and $0$ otherwise.

$$
\varepsilon^{\mu\nu\rho\sigma} = \varepsilon_{\mathrm{symbol}}(\mu,\nu,\rho,\sigma)
$$

Aliases: Levi-Civita symbol; Levi-Civita tensor components

Queries: “What are the components of the Levi-Civita tensor in the standard basis of real Lorentz tensors?” · “\varepsilon^{\mu\nu\rho\sigma} \text{ components equal the Levi-Civita symbol}” · “Levi-Civita tensor basis representation leviCivitaSymbol Fin 4 Lorentz”

