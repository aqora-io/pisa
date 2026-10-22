# Full informalization run, 2026-10-22

- Environment: `v4.35.0-da683984`
- Model: `claude-haiku-5-5`, effort `low`, JSON-schema output; the instructions and glossary are prompt-cached
- Sample: all of Mathlib, Physlib, Batteries, Init, Std, QuantumInfo
- 69 Physlib informal results in the sample took their docstring as the description (no model call)

## Cost

| | |
|---|---|
| Calls | 127811 (126732 described, 1079 failed) |
| Input tokens: uncached / cache write / cache read | 133569519 / 69511 / 344659672 |
| Output tokens (with thinking) | 41820166 |
| Cost | $37.722 ($0.00030 per description) |
| Wall time | 11341 s at concurrency 40 |

### Full run estimate

Declarations that need a description and had none stored before this run (LeanSearch v2 covers much of Mathlib), at this pilot's mean cost per description. The Message Batches API halves it.

| Library | Declarations | Messages API | Batches API |
|---|---:|---:|---:|
| Batteries | 3460 | $1.03 | $0.51 |
| Init | 31717 | $9.44 | $4.72 |
| Mathlib | 54637 | $16.26 | $8.13 |
| Physlib | 10994 | $3.27 | $1.64 |
| QuantumInfo | 9 | $0.00 | $0.00 |
| Std | 26994 | $8.03 | $4.02 |
| **All** | **127811** | **$38.04** | **$19.02** |

## Failures

- `AlgebraicGeometry.Scheme.Hom.asFiberHom`: empty description
- `AbsolutelyContinuousOnInterval.uniformContinuousOn`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `AddAction.properVAdd_of_proper_orbitMap`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Abelian.SpectralObject`: empty description
- `CategoryTheory.InjectivePresentation.shortComplex`: empty description
- `CategoryTheory.NatTrans.CommShift.rightDerivedNatTrans`: empty description
- `CategoryTheory.ObjectProperty.IsClosedUnderFiniteColimits`: empty description
- `CategoryTheory.ObjectProperty.IsStableUnderRetracts.instIsClosedUnderIsomorphisms`: empty description
- `CategoryTheory.Limits.reflectsLimit_unop`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CommRingCat.Opposite.regularEpiOfFaithfullyFlat`: empty description
- `ContDiffMapSupportedIn.continuous`: empty description
- `ContinuousAlgHom.eqOn_closure_adjoin`: empty description
- `CategoryTheory.PreGaloisCategory.instIsPretransitiveAutObjFiniteVFintypeCatFunctorObjActionFunctorToActionOfIsGalois`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ContinuousLinearMap.continuous_toLinearMap`: empty description
- `ConvexOn.continuousOn_intrinsicInterior`: empty description
- `ConvexOn.continuousOn_Ici`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `IsPGroup.card`: empty description
- `Function.instIsTransOnFun`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `IsValuativeTopology.isTopologicalRing`: empty description
- `HasEquiv`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Group.of_quotient_center_nilpotent`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `MeasureTheory.StronglyMeasurable.oneLePart`: empty description
- `ProfiniteAddGrp.toFiniteQuotientFunctor`: empty description
- `RingHom.Smooth.formallySmooth`: empty description
- `SimpleGraph.Adj.isPath_toWalk`: empty description
- `WithBot.isLUB_sSup'`: empty description
- `continuousOn_uIcc_extendFrom_uIoo`: empty description
- `dualTensorHom_bijective_of_finite_left_projective_right`: empty description
- `eqOn_finsetProd`: empty description
- `Subtype`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `isTopologicalGroup_induced`: empty description
- `AddCommute.vadd_left_iff`: empty description
- `AddCommute.vadd_right_iff`: empty description
- `AddMonoidAlgebra.mapDomainBialgHom_mapDomainOfBialgHom`: empty description
- `Absorbent.eq_univ_of_smulMemClass`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Acc.below`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Acc.ndrec`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Acc.inv`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Algebra.IsFiniteSplit.of_algEquiv`: empty description
- `Algebra.WeaklyEtale.flat_lmul'`: empty description
- `AlgebraicGeometry.Scheme.Modules.map_comp_smul`: empty description
- `AlgebraicGeometry.Scheme.Hom.isIrreducible_preimage`: empty description
- `AlgebraicGeometry.morphismRestrict_morphismRestrict_ι_isoImage_hom`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Batteries.AssocList.any`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Bimon.equivMonComonUnitIsoAppXAux_hom`: empty description
- `CategoryTheory.Equivalence.cancel_unit_left`: empty description
- `CategoryTheory.Functor.IsCoverDense.restrictHomEquivHom_naturality_right_symm`: empty description
- `CategoryTheory.Functor.IsCoverDense.Types.naturality`: empty description
- `CategoryTheory.Comma.fstSndJointlyReflectLimit`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Comma.fstSndJointlyReflectColimit`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Iso.inv_hom_id_map_assoc`: empty description
- `CategoryTheory.Limits.BinaryCofan.ext_inv_hom`: empty description
- `CategoryTheory.Limits.HasPushout.comp_left_left_iff_of_isIso`: empty description
- `CategoryTheory.GrothendieckTopology.overMapPullbackComp_hom_app_hom_app`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Limits.HasWeakLimitsOfShape.hasWeakLimit`: empty description
- `CategoryTheory.MonoidalCategory.pentagon_hom_hom_inv_inv_hom_assoc`: empty description
- `CategoryTheory.MonoidalCategory.pentagon_inv_inv_hom_hom_inv_assoc`: empty description
- `CategoryTheory.MorphismProperty.pullbackLift_fst_snd`: empty description
- `CategoryTheory.MorphismProperty.Comma.mapRightId_hom_app_right`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.PreOneHypercover.sieve₀_map`: empty description
- `CategoryTheory.NatIso.inv_map_hom_app`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.ShortComplex.leftRightHomologyComparison'_eq_leftHomologyMap'_comp_iso_hom_comp_rightHomologyMap'`: empty description
- `CategoryTheory.ShortComplex.π_moduleCatCyclesIso_hom_apply`: empty description
- `CategoryTheory.Subfunctor.eq_sheafify`: empty description
- `CategoryTheory.Triangulated.TStructure.instIsIsoMapTruncGEAppTruncGEπ`: empty description
- `CategoryTheory.Triangulated.TStructure.truncLT_map_truncLTι_app`: empty description
- `CategoryTheory.exists_eq_of_isCofiltered_costructuredArrow`: empty description
- `CategoryTheory.instIsCardinalPureComp`: empty description
- `CategoryTheory.ShortComplex.moduleCatCyclesIso_hom_i_apply`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.plusPlusSheaf_map_hom`: empty description
- `CliffordAlgebra.equivOfIsometry_trans`: empty description
- `Coalgebra.IsFrobenius.right_def`: empty description
- `CoassocSimps.assoc_comp_map_map_comp_assoc`: empty description
- `Computation.eq_of_bisim`: empty description
- `CategoryTheory.trifunctorComp₃₄FunctorObj`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CommMonCat.coyonedaForget_hom_app_app_hom_apply_hom`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Filter.tendsto_comp_card_atTop_iff`: empty description
- `FintypeCat.toProfinite_obj`: empty description
- `Differential.equiv`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `FirstOrder.Language.presburger.isSemilinearSet_formula_realize_semilinear`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `HomologicalComplex.quasiIsoAt_π_of_isLimit_of_isEventuallyConstantTo`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `HomotopicalAlgebra.PrepathObject.toFullSubcategory_ι`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `InformationTheory.toReal_klDiv_map_of_ac`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lean.Grind.CommRing.Expr.pow`: empty description
- `List.takeD`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Matroid.eq_restrict_removeLoops`: empty description
- `MeasurableSpace.comap_process_pi`: empty description
- `MeasureTheory.nullMeasurableSet_iff_eventuallyMeasurableSet`: empty description
- `Mathlib.Tactic.Echelon.Model.ops`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Module.isTorsionFree_int_iff_hasUniqueDiv`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `NonUnitalRingHom.toAddMonoidHom_comp`: empty description
- `NonUnitalStarAlgHom.coe_ofClass`: empty description
- `OrderDual.instIsRightCancelMulZero`: empty description
- `OrderDual.instIsCancelMul`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Polynomial.isRegularRing_of_isRegularRing`: empty description
- `Physlib.Wirtinger.fderiv_comp_clm_apply`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Polynomial.aevalAeval_C`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `RelIso.relEmbeddingCongr_apply_apply`: empty description
- `RingHom.IsPurelyInseparable.comp`: empty description
- `RootPairing.ext`: empty description
- `SSetPair`: empty description
- `Set.Finite.ecard_strictMonoOn`: empty description
- `Rep.instReflectsLimitsOfSizeModuleCatForget₂IntertwiningMapVρLinearMapIdCarrier`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Http.Config.maxChunkExtNameLength`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Http.Protocol.H1.Error.tooManyHeaders`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Iterators.Types.ArrayIterator.exists_iff`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Std.Iterators.ULiftT`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Submodule.FG.cofg_of_isCompl`: empty description
- `Std.Time.DateFormatSymbols.amShort`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Sym.instFinite`: empty description
- `Subrel.instWellFoundedSubtype`: empty description
- `Subgroup.isFiniteRelIndex_top_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Topology.CWComplex.Subcomplex.copy_eq`: empty description
- `Submodule.orthogonalBilin_map`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `UniformContinuous.prod_closeds`: empty description
- `dualTensorHom_self_right`: empty description
- `inner_conj_symm'`: empty description
- `eq_of_clusterPt_uniformity`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `tendsto_atBot_of_antitone`: empty description
- `type_eq_of_heq`: empty description
- `ACCSystemCharges.Charges`: empty description
- `Algebra.HasGoingUp.exists_ideal_ge_liesOver_of_lt`: empty description
- `AlgebraicGeometry.Scheme.Hom.resLE_app_top`: empty description
- `AlgebraicTopology.DoldKan.Compatibility.equivalence₁_functor`: empty description
- `Array.Perm.trans`: empty description
- `Bundle.Trivialization.contMDiffOn_symm`: empty description
- `CategoryTheory.AddGrp.tensorAddUnit_add`: empty description
- `CategoryTheory.AddMonObj.zero_associator`: empty description
- `CategoryTheory.Arrow.Hom.w_assoc`: empty description
- `CategoryTheory.CardinalDirectedPoset.Hom.injective`: empty description
- `ByteArray.hash`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Limits.equalizerPullbackMapIso_inv_ι_snd`: empty description
- `CategoryTheory.Limits.equalizerPullbackMapIso_hom_snd_assoc`: empty description
- `CategoryTheory.Functor.PullbackObjObj.ofIsInitial_fst`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Functor.PushoutObjObj.mapArrowLeft`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Limits.pushoutIsoUnopPullback_inr_hom_assoc`: empty description
- `CategoryTheory.Localization.SmallHom.equiv_shift`: empty description
- `CategoryTheory.Mathlib.Tactic.MonTauto.add_assoc_neg_assoc`: empty description
- `CategoryTheory.MonoidalCategory.ofBifunctor.Triangle.leftUnitorMap_app_app`: empty description
- `CategoryTheory.GrpObj.ofInvertible`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.MorphismProperty.instRespectsLeftOfObjectPropertyIsomorphismsOfIsClosedUnderIsomorphisms`: empty description
- `CategoryTheory.Limits.BinaryCofan.map_inl`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Limits.BinaryCofan.map_inr`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.ObjectProperty.instContainsZeroMinOfIsClosedUnderIsomorphisms_1`: empty description
- `CategoryTheory.prod.prodμ_inverse_obj`: empty description
- `CategoryTheory.yonedaEquiv_symm_app`: empty description
- `CompactCoherentification.continuous_dom_of_continuous`: empty description
- `Complex.UnitI`: empty description
- `Con.congr_symm`: empty description
- `CategoryTheory.equivYoneda'`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.hasCardinalLT_arrow_iff_of_isThin`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ContinuousMap.eventually_range_subset`: empty description
- `Convexity.IsAffineMap.neg`: empty description
- `ContinuousAlternatingMap.cpolynomialOn_compContinuousLinearMapCLM`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `EuclideanGeometry.Sphere.inter_orthRadius_eq_empty_of_radius_lt_dist`: empty description
- `Equiv.Perm.eq_sign_of_surjective_hom`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Finsupp.image_sumElim_product_antidiagonal`: empty description
- `Float.atan`: empty description
- `Float32.asinh`: empty description
- `Function.HasFiniteMulSupport.fun_comp_of_injective`: empty description
- `Function.HasFiniteSupport.fun_comp_of_injective`: empty description
- `Function.support_along_fiber_finite_of_finite`: empty description
- `Function.IsConst.comp_left`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `IsGLB.exists_of_nonempty_of_not_isPredLimit`: empty description
- `IsLocalRing.ResidueField.mapAlgEquiv_residue`: empty description
- `IsMulFG.of_surjective`: empty description
- `IsSumNonzeroSq.isSumSq`: empty description
- `Lean.Sym.BitVec.eq_eq_false`: empty description
- `LieAlgebra.Basis.nondegen`: empty description
- `LinearMap.eval_separatingRight`: empty description
- `List.isPrefixOf_cons_cons_self`: empty description
- `Matrix.IsHadamard`: empty description
- `Matroid.IsBasis'.contract_isBasis'_sdiff_sdiff_of_subset`: empty description
- `Matroid.closure_sdiff_eq_of_subset_coloops`: empty description
- `MeasureTheory.Measure.ae_eq_compProd_of_ae_eq_fst`: empty description
- `Module.Free.exists_linearMap_injective_of_lift_rank_lt`: empty description
- `MonoidAlgebra.map_injective`: empty description
- `MeasureTheory.ae_bdd_abs_condExp_of_ae_bdd_abs`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `MeasureTheory.lintegral_condLExp`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.instLawfulBEq`: empty description
- `MonoidHom.eqLocus`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `MonoidHom.isMulCommutative_of_isCyclic_of_ker_le_center`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `OptionT.instLawfulMonad`: empty description
- `Nat.lt_irrefl`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.ne_of_beq_eq_false`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `NumberField.ComplexEmbedding.disjoint_unmixedEmbeddingsOver_mixedEmbeddingsOver`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Pi.separatelyContinuousAdd`: empty description
- `OrderHom.instIsOneApplyEqSelf`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `PresheafOfModules.ModuleColimit.jointly_surjective₃`: empty description
- `ProbabilityTheory.IsArgminEstimator.property`: empty description
- `PartialHomeomorph.toPartialEquiv_injective`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Physlib.CondensedMatter.Crystal.nonemptyUnitCell`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `RBTree.RBNode.WF.out`: empty description
- `RKHS.continuous_eval_const`: empty description
- `RCLike.iInter_countable_halfSpaces_eq`: empty description
- `Rep.epi_iff_surjective`: empty description
- `Rep.ihom_map`: empty description
- `RingCon.dfinsuppSumAddHom`: empty description
- `RBTree.RBNode.zoom`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Rat.not_le`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SM.SMNoGrav.One.linearParameters.ext_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Set.powersetCard.isPretransitive_of_isMultiplyPretransitive'`: empty description
- `SimpleGraph.Walk.nil_drop_of_length_le`: empty description
- `SkewPolynomial.coeff_neg`: empty description
- `StarAlgEquiv.ofNonUnitalStarAlgHom_symm_apply`: empty description
- `SheafOfModules.pushforwardPushforwardEquivalence_unit_app_val_app`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SheafOfModules.ιFree`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `StateTransition.eval`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Std.HashSet.Raw.Equiv.union_left`: empty description
- `Std.ExtTreeMap`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Http.Protocol.H1.Machine.processWrite`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Std.LawfulCmp.toTransCmp`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Std.Rio.iter`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Slice.Self.eq`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Time.GenericFormat.string`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Std.Time.DateTime.offset`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Subgroup.Commensurable.comap_surjective_iff`: empty description
- `Subgroup.eq_of_index_dvd_index`: empty description
- `Subgroup.isNilpotent_of_ker_le_center`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SuccOrder.accPt_principal`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SuccOrder.nhds_bot`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SuccOrder.nhds_of_isMin`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Topology.ContinuousGeneratedBy.comp`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `WeierstrassCurve.Affine.equation_add`: empty description
- `UInt8.instLawfulUpwardEnumerableLT`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `UInt8.instLawfulUpwardEnumerableLeast?`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `UInt8.le_antisymm`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `WithZero.le_unzeroD`: empty description
- `Vector.lawfulBEq_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `directedOn_or_directedOn_of_union'`: empty description
- `equicontinuousWithinAt_unique`: empty description
- `WithTopology.equiv`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `isCoboundedUnder_ge_min`: empty description
- `intermediate_value_Iio`: empty description
- `instContinuousConstSMulConjActOfSeparatelyContinuousMul`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `sSetTopAdj_unit_app_app_down`: empty description
- `ACCSystemCharges.chargesAddCommMonoid_zero_num`: empty description
- `AddCommute.addConj_iff`: empty description
- `AddMonoidAlgebra.mapDomainBialgHomEquiv_apply`: empty description
- `AddSemiconjBy.addConj_iff`: empty description
- `AddSubgroup.eq_zero_of_noncommSum_eq_zero_of_iSupIndep`: empty description
- `AddSubgroup.le_normalizer_iff`: empty description
- `AddSubsemigroup.coe_iSup_eq_iUnion_finset_coe_biSup`: empty description
- `AddMonoidAlgebra.support_coeff_single_mul`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Algebra.isLocalIso_iff`: empty description
- `AlgebraicGeometry.Scheme.Hom.singleton_mem_qcPrecoverage`: empty description
- `AlgebraicGeometry.Scheme.PartialMap.equiv_of_restrict_eq`: empty description
- `Array.array_mkSlice_rci`: empty description
- `Array.pairwise_push`: empty description
- `AugmentedSimplexCategory.equivAugmentedSimplicialObjectFunctorCompDropIso_hom_app_app`: empty description
- `Batteries.BinomialHeap.Imp.HeapNode.realSize_toHeap`: empty description
- `Batteries.BinaryHeap.mem_empty`: empty description
- `Batteries.Tactic.Lint.LintVerbosity.ofNat`: empty description
- `BiLinearSymm.map_add₂`: empty description
- `BoundedVariationOn.continuousWithinAt_variationOnFromTo_inter_Iic`: empty description
- `BoundedVariationOn.tendsto_eVariationOn_Icc_zero_right`: empty description
- `Bialgebra.counit_eq_zero_of_comul_eq_tmul_add_tmul`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CFC.concaveOn_nnrpow`: empty description
- `CategoryTheory.Abelian.Ext.eq_zero_of_hasProjectiveDimensionLT`: empty description
- `CategoryTheory.Abelian.isIso_of_epi_of_isIso`: empty description
- `Cardinal.ofNat_eq_toENat`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Abelian.AbelianStruct.fac`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Abelian.AbelianStruct.imageι_π`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CartanMatrix.Realisation.sRoot`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Functor.instAdditiveOfNat`: empty description
- `CategoryTheory.CardinalDirectedPoset.withTop`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Limits.IsWeakLimit.ofIsoWeakLimit_lift`: empty description
- `CategoryTheory.GrothendieckTopology.isColimitCofanMkYoneda`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.GrothendieckTopology.ofArrows_mem_iff_isLocallySurjective_cofanIsColimitDesc_shrinkYoneda_map`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Preadditive.RightFreyd.instHasCokernels`: empty description
- `CategoryTheory.Limits.createsFiniteCoproductsUnop`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Presieve.overEquiv_symm_apply`: empty description
- `CategoryTheory.Pseudofunctor.IsStackFor.essSurj`: empty description
- `CategoryTheory.Triangulated.TStructure.isGE_iff_orthogonal`: empty description
- `CategoryTheory.Triangulated.TStructure.isLE_truncLT_obj`: empty description
- `Coalgebra.Repr.tmul_index`: empty description
- `ClassicalMechanics.PointParticle.NewtonianSystem.particles`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ContMDiffSMul.contMDiff_const_smul`: empty description
- `ComplexShape.χ_prev`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ComplexShape.spectralSequenceNat`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ContinuousLinearMap.toLinearEquiv_toSpanSingletonLIE`: empty description
- `ConvexOn.exists_affine_le_real`: empty description
- `Continuous.fun_pow`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ContinuousAt.isTheta`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `DFinsupp.sum_finsetSum_index`: empty description
- `DirectedOn.notMem_of_lt_csInf`: empty description
- `Filter.HasBasis.equicontinuousWithinAt_iff_right`: empty description
- `Except.map_pmap`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Finset.HasMulAntidiagonal.nonempty_antidiagonal`: empty description
- `Filter.Tendsto.sdiv`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Finset.HasMulAntidiagonal.mem_mulAntidiagonal_ofAdd_iff_toAdd_mem_antidiagonal`: empty description
- `Finset.powersetCard_injOn`: empty description
- `Finset.SupIndep.insert`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `FunLike.coe_smul`: empty description
- `Function.HasFiniteSupport.mul_right`: empty description
- `FunctionField.instFiniteDimensionalSubtypeMemIntermediateFieldOfIsAlgebraic`: empty description
- `Frullani.tendsto_integral_inv_smul_nhdsWithin`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Graph.IsClosedSubgraph.closed`: empty description
- `HasFDerivAt.eventually_notMem`: empty description
- `HasFDerivWithinAt.eventually_notMem`: empty description
- `Function.uncurry_apply_pair`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Group.isSolvable_iff_commutator_lt`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Group.IsNilpotent.exists_normal_card_eq_of_dvd_card`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Hypergraph.mem_vertexSet_of_mem_edgeSet`: empty description
- `ISize.neg_one_shiftLeft_and_shiftLeft`: empty description
- `HasStandardEtaleSurjectionOn.mk`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Ideal.IsInertiaRing.toIsGaloisGroup`: empty description
- `Ideal.Quotient.map_ker_stabilizer_subtype`: empty description
- `Ideal.mem_inertia`: empty description
- `ISize.shiftRight_xor`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Inclusion.IntervalDyadicReal.Iic_mem_of_mem_Iic`: empty description
- `InnerProductGeometry.norm_sq_add_norm_sub_add_sq_eq_norm_sub_sq_add_norm_sub_sq_iff_inner_eq_zero`: empty description
- `Int.cast_eq`: empty description
- `Int.lt_ediv_of_mul_lt`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Int.mul_ediv_mul_of_pos`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Int16.left_eq_add`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Int64.or_neg_one`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Int8.add_zero`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `IsCompact.tendsto_nhdsSet_of_mapClusterPt`: empty description
- `IsExposed.eq_inter_halfSpace'`: empty description
- `IsIrreducible.preimage_of_isPreirreducible_fiber`: empty description
- `IsKleinFour.eq_finset_univ`: empty description
- `IsLocalization.isMaximal_iff_isMaximal_disjoint`: empty description
- `IsNoetherian.equivPUnitOfProdInjective_apply`: empty description
- `IsNatCastApplyEqSMul.natCast_apply_eq_smul`: empty description
- `IsPicardLindelof.shrink`: empty description
- `IsSelfAdjoint.I_smul_of_mem_skewAdjoint`: empty description
- `IsSelfInv.neg`: empty description
- `IsVAddRegular.of_vadd`: empty description
- `IsValuativeTopology.continuousAt_valuation_of_ne_zero`: empty description
- `Lean.Data.AC.Context.evalList_sort_congr`: empty description
- `LawfulMonadStateOf.set_bind_modifyGet`: empty description
- `IsPrimitiveRoot.card_rootsOfUnity'`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Lean.Grind.AC.Seq.sort_k`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lean.Grind.Order.lt_eq_false_of_le_k`: empty description
- `Lean.Grind.AddCommGroup.add_sub_cancel`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lean.Grind.Linarith.instAssociativeHAdd_1`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lean.Grind.Linarith.eq_normN`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lean.Order.List.monotone_anyM`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lean.Meta.Simp.defaultMaxSteps`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `LieRinehartRing.lie_smul_eq_mul'`: empty description
- `LinearEquiv.dilatransvection`: empty description
- `LinearIndependent.update`: empty description
- `LinearMap.dualEmbedding_injective_of_separatingRight`: empty description
- `LinearMap.exact_smul_id_smul_top_mkQ`: empty description
- `LinearPMap.HasDenseDomain.smul`: empty description
- `List.eraseP_ne_nil_iff`: empty description
- `List.find?_range_eq_some`: empty description
- `List.eq_nil_or_concat'`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `List.getElem_intersperse_two_mul`: empty description
- `List.isEmpty_nil`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `List.subset_append_right`: empty description
- `List.take_append_drop`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Mathlib.Meta.NormNum.isNNRat_divNat`: empty description
- `Mathlib.Meta.NormNum.isNat_nnrealSqrt`: empty description
- `Matrix.ProjectiveSpecialLinearGroup.rank_two_simple'`: empty description
- `Matrix.card_SL`: empty description
- `Matrix.SpecialLinearGroup.diagonal_neZero`: empty description
- `Matrix.toLpLin_eq_toLin`: empty description
- `Matrix.SpecialLinearGroup.discreteSpecialLinearGroupIntRangeSL`: empty description
- `Matrix.IsFiniteCartan.diag`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `MeasureTheory.FinMeasAdditive.add_right_measure`: empty description
- `MeasureTheory.Measure.eq_rnDeriv`: empty description
- `MeasureTheory.VectorMeasure.ext_of_generateFrom`: empty description
- `MeasureTheory.ae_finsetSum_measure_iff`: empty description
- `MeasureTheory.instSFiniteHSMulMeasure`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Metric.exists_forall_closedEBall_subset_aux₁`: empty description
- `ModelWithCorners.contDiffWithinAt_extendCoordChange'`: empty description
- `ModelWithCorners.contDiffOn_extendCoordChange_symm`: empty description
- `Memℓp.holder_top_right_bound`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Module.Basis.span_repr_eq_single`: empty description
- `Module.Projective.of_one_mem_range_dualTensorHom`: empty description
- `Monoid.CoprodI.Word.equivPair_eq_of_fstIdx_ne`: empty description
- `MonoidHom.submonoidComap_injective`: empty description
- `Module.Invertible.exists_finset_free_localization`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `MvPolynomial.mem_pow_idealOfVars_iff'`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.cast_mem_Iic_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.induct_roc_left`: empty description
- `Nat.lt_of_sub_eq_succ`: empty description
- `Nat.ext_div_modEq_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.le_sub_iff_add_le'`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `NonUnitalClosedEmbeddingContinuousFunctionalCalculus.isClosedEmbedding`: empty description
- `Nat.shiftRight_succ`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.shiftRight_succ_inside`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.sub_lt_iff_lt_add'`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `OptionT.run_monadMap`: empty description
- `OmegaCompletePartialOrder.ContinuousHom.ofFun_apply`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Ordinal.CNF.eval`: empty description
- `Ordinal.IsFundamentalSeq.strictMono`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `OrthonormalBasis.norm_dual`: empty description
- `OrderIso.toRelIsoGT_symm`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Pell.eq_pell_lem`: empty description
- `Pell.eq_pow_of_pell`: empty description
- `PolyEquivTensor.toFunLinear_mul_tmul_mul_aux_2`: empty description
- `Polynomial.eq_zero_of_homogenize_eq_zero`: empty description
- `PrimeSpectrum.equivSubtype_symm_apply_asIdeal`: empty description
- `PresheafOfModulesOfCommRing.HasDifferentials.exists_universal_derivation`: empty description
- `ProbabilityTheory.Kernel.coe_nsmul`: empty description
- `RBTree.RBNode.All.lowerBound?_lb`: empty description
- `QuadraticForm.isometryEquivWeightedSumSquaresWeightedSumSquares`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Real.differentiableAt_inv_log`: empty description
- `Rep.FiniteCyclicGroup.groupHomologyπEven_eq_zero_iff`: empty description
- `Rep.nsmul_hom`: empty description
- `Rat.inv_pos`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Representation.IsIrreducible.bijective_or_eq_zero`: empty description
- `RootPairing.eq_baseOf_of_linearIndepOn_of_mem_or_neg_mem_closure`: empty description
- `RootPairing.exists_pos_pairingIn`: empty description
- `RootPairing.exists_coroot_sub_coroot_eq_zsmul`: empty description
- `SFinKer.whiskerLeft_hom`: empty description
- `SSet.Subcomplex.existsN`: empty description
- `SSet.opObjEquiv_mem_nonDegenerate_iff`: empty description
- `Representation.IntertwiningMap.lTensor_add`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SSet.relativeCellComplexOfMono.Cell.ιSigmaStdSimplex`: empty description
- `SaturatedAddSubmonoid.mem_sInf`: empty description
- `ScottContinuousOn.image_comp`: empty description
- `Set.IsSelfInv.of_inv_subset`: empty description
- `Setoid.eqv_class_mem'`: empty description
- `Set.eq_univ_iff_forall`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Set.exists_mem_univ`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SimpleGraph.Walk.isHamiltonian_transfer`: empty description
- `SimpleGraph.Walk.isTrail_concat`: empty description
- `SimpleGraph.Walk.nil_take_iff`: empty description
- `SimpleGraph.cliqueFree_hasse_three`: empty description
- `SimpleGraph.IsEdgeConnected.le_degree`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SimpleGraph.IsEdgeReachable.le_degree`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Squarefree.dvd_of_isSquare_mul`: empty description
- `Std.DTreeMap.Internal.Impl.Const.getEntryLE?_eq_map`: empty description
- `Std.DTreeMap.Internal.Impl.Equiv.constGetEntryGT?_eq`: empty description
- `Std.DHashMap.Equiv.congr_left`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `StateTransition.Reaches₁`: empty description
- `Std.DHashMap.Raw.mem_diff_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.Raw.WF.markLinear`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.WF.filter`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.getKey!_map`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashMap.Equiv.diff_left`: empty description
- `Std.HashMap.Equiv.getElem!_eq`: empty description
- `Std.Do.SPred.ext_nil`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Refl.set_pairwise_iff`: empty description
- `Std.ReflOrd.isLE_rfl`: empty description
- `Std.Iterators.Types.Zip.instProductive`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Iterators.PostconditionT.pbind`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Subgroup.characteristic_sInf`: empty description
- `String.Pos.lt_of_le_of_ne`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `String.Slice.Pattern.SearchStep.endPos_rejected`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Submonoid.instIsMulCommutative_closure`: empty description
- `TensorProduct.eq_zero_of_fg_of_subtype_eq_zero`: empty description
- `Sum.liftRel_swap_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `TopologicalSpace.IsOpenCover.exists_finite_of_compactSpace`: empty description
- `TopologicalSpace.IsTopologicalBasis.continuousOn_iff`: empty description
- `TopologicalSpace.IsTopologicalBasis.isTopologicalBasis_of_exists_subset`: empty description
- `Topology.CWComplex.nonempty_cellFrontier`: empty description
- `TensorSpecies.Tensor.ComponentIdx`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `UInt8.or_eq_zero_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `UInt8.zero_xor`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `ValuativeRel.veq_mul_right_comm`: empty description
- `UpperHalfPlane.glScalar_smul`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `WeierstrassCurve.Jacobian.map_nonsingular`: empty description
- `Wick.koszulSignInsert_ge_forall_append`: empty description
- `alternatingGroup.normal_subgroup_eq_bot_or_eq_top_of_card_ne_eight`: empty description
- `WithAbs.toAbs_zero`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `WithAbs.toAbs_sub`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `WithBot.bot_lt_one`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `WithBot.eq_bot_iff_forall_ne`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `contMDiffWithinAt_finsetSum'`: empty description
- `dirSupInacc_iff_inter_subset`: empty description
- `dist_homothety`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `exists_isLocalExtr_uIoo_of_tendsto`: empty description
- `exists_nnnorm_ne_zero'`: empty description
- `instAssociativeUInt16HAdd`: empty description
- `finsum_mem_inter_add_sdiff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `intervalIntegral.integral_comp_mul_deriv_of_deriv_nonneg`: empty description
- `invOf_eq_iff_left`: empty description
- `isCompactElement_iff_le_of_directed_sSup_le`: empty description
- `isIdempotentElem_star_mul_self_iff_isIdempotentElem_self_mul_star`: empty description
- `isLocallyClosed_iff_isOpen_preimage_val_closure`: empty description
- `instCommutativeInt8HOr`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `instLawfulIdentityISizeHAddOfNat`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `lowerHemicontinuousAt_singleton_iff`: empty description
- `isRegular_natCast`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `mvfderivWithin_comp_of_preimage_mem_nhdsWithin_of_eq`: empty description
- `mvfderiv_fun_mul`: empty description
- `summable_sum_mul_antidiagonal_of_summable_mul`: empty description
- `qrSign.eq_iff_eq`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `AddSubmonoid.AddSaturated.inf`: empty description
- `AdjoinRoot.equiv'_symm_toAlgHom`: empty description
- `Affine.Triangle.eulerPoint_eq_midpoint`: empty description
- `AddAut.conj_apply`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Array.eraseP`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `BoundingSieve.selbergTerms_isMultiplicative`: empty description
- `CategoryTheory.Abelian.SpectralObject.isIso_H_map_twoδ₁Toδ₀`: empty description
- `CategoryTheory.AddGrpObj.addConj`: empty description
- `CategoryTheory.AddGrpObj.comp_sub`: empty description
- `CategoryTheory.AddMod.Hom.mk'_hom`: empty description
- `CategoryTheory.Codiscrete.uniqueHom_default`: empty description
- `CKMMatrix.fst_row_normalized_abs`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CKMMatrix.fst_row_normalized_normSq`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CantorBendixson.iteratedDerivedSet_mem_fixedPoints`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Functor.RightExtension.nonempty_isPointwiseRightKanExtensionAt_compTwoSquare_iff`: empty description
- `CategoryTheory.GrpObj.η_whiskerRight_commutator_assoc`: empty description
- `CategoryTheory.InducedCategory.eqToHom_hom`: empty description
- `CategoryTheory.IsHomLift.eqToHom_comp_lift_iff`: empty description
- `CategoryTheory.Limits.parallelPair.parallelPairHom_left`: empty description
- `CategoryTheory.RingObjCat.forget₂AddMon_map_hom`: empty description
- `CategoryTheory.Limits.SingleObj.Types.sections.equivFixedPoints_apply_coe`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Limits.biprod.inl_opIso_inv_assoc`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Span.SpanBicat.mkHom₂_hom`: empty description
- `CategoryTheory.Sum.Swap.equivalenceFunctorEquivFunctorIso`: empty description
- `CategoryTheory.Over.eqToHom_left`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Triangulated.Octahedron'.m₃`: empty description
- `ClassGroup.mulEquiv`: empty description
- `Coalgebra.Repr.mul_index`: empty description
- `ContinuousLinearMap.IsFredholm.index_continuousAt`: empty description
- `ConvexOn.exists_affine_le_of_lt`: empty description
- `ClosedSubmodule.symplComp`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `DiffeologicalSpace.CorePlotsOn.locality`: empty description
- `Equiv.Finset.congr`: empty description
- `CountablyCompactSpace.isCountablyCompact_univ`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Fermion.RightHandedWeyl.toFin2ℂFun`: empty description
- `FieldStatistic.ofList_take_insert`: empty description
- `Filter.EventuallyEmptyOrUniv.indicator_const`: empty description
- `Fin.decodeFun_encodeFun`: empty description
- `FundamentalGroupoid.conj_eqToHom_assoc`: empty description
- `Ideal.isAugmentation_subalgebra_iff`: empty description
- `Ideal.primaryComponent_torsionBySet_eq_inf`: empty description
- `GalileanGroup.spaceTranslation`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `GalileanGroup.timeTranslation`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Ideal.primaryComponent_map_mem`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Ideal.primaryComponent_sup`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Int.gcd_sub_right_left_of_dvd`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `IsCompact.isOpen_constructibleTopology_of_isOpen`: empty description
- `IsAddTorsion.torsionAddEquiv_apply`: empty description
- `IsCyclotomicExtension.Rat.mem_zpowers_galEquivZMod_of_mem_stabilizer`: empty description
- `IsSeqCompact.isCountablyCompact`: empty description
- `IsUpperSet.dirSupClosedOn`: empty description
- `Lean.Grind.CommRing.Power.revlex`: empty description
- `Lean.Grind.leftInv`: empty description
- `Lean.Grind.CommRing.Mon.revlexFuel.induct_unfolding`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `LieSubmodule.map_restrictLie_incl_top`: empty description
- `List.disjoint_append_left`: empty description
- `LinearMap.liftQ₂_mk`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `List.min_eq_max`: empty description
- `List.getLast_eraseP_mem`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lorentz.SL2C.boostAxis_two_apply`: empty description
- `List.pairwise_le_range'`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `List.prod_flatten`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Mathlib.Meta.NormNum.irrational_rpow_rat_rat_of_num`: empty description
- `List.satisfiesM_foldrM`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `List.rightpad_toArray`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Mathlib.Tactic.Ring.Common.Cache.dsα`: empty description
- `Matrix.IsDetpBalanced.trans`: empty description
- `Lorentz.SL2C.boostAxis_inv`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `MaxTropical.tropEquiv_symm_coe_fn`: empty description
- `Module.fgSystem.equiv_comp_of`: empty description
- `MonoidWithZeroHom.ValueGroup₀.zero_or_exists_mk`: empty description
- `MeasurableEmbedding.equivRange`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.recAux_zero`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `PartialEquiv.toEquiv_eq_codRestrict_restrict`: empty description
- `NormedSpace.toLinearMap_inclusionInDoubleDualWeak`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `PointedCone.IsFaceOf.snd`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Polynomial.derivative_physHermite_succ`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ProbabilityTheory.bernoulliMeasure_apply_of_mem_of_mem`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SSet.Subcomplex.N.orderIsoOfIso_symm_apply`: empty description
- `SSet.Subcomplex.unionProd.ι₂_ι_assoc`: empty description
- `Set.Icc.convexComb_le`: empty description
- `Set.powersetCard.coe_compl`: empty description
- `Set.Finite.isCountablyCompact_sUnion`: empty description
- `Set.OrdConnected.domRestrict`: empty description
- `SimpleGraph.eq_starGraph_of_isUniversal_of_cliqueFree_three`: empty description
- `SimplexCategory.δ₀Iter_zero`: empty description
- `SimplexCategoryGenRel.eq_or_len_le_of_P_δ`: empty description
- `StateTransition.reaches_eval`: empty description
- `SimpleGraph.Embedding.toHom_sum`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SimpleGraph.Subgraph.coeEmbeddingSpanningCoe_apply`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SimpleGraph.Reachable.of_isUniversal`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.Raw.isEmpty_diff_iff`: empty description
- `Std.DTreeMap.Internal.Impl.Const.size_filterMap_eq_size_iff`: empty description
- `Std.DTreeMap.Internal.Impl.getKey_filter!`: empty description
- `Std.DTreeMap.Internal.Impl.get_insert!_self`: empty description
- `Std.DTreeMap.Internal.Impl.maxKey?_alter!_eq_self`: empty description
- `Std.DTreeMap.Raw.Equiv.diff_left`: empty description
- `Std.DTreeMap.Internal.Impl.mem_toList_iff_get?_eq_some`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Do.SPred.and_or_elim_l`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Rii.isEmpty_iff`: empty description
- `Std.Sat.AIG.IsPrefix.idx_eq`: empty description
- `Std.Iterators.HetT.property_lift`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeMap.Raw.getElem!_inter_of_mem_right`: empty description
- `Std.WP.Spec.restoreM_trans`: empty description
- `Std.WP.StateM.of_run_eq_wp`: empty description
- `Std.WP.WPMonad.wp_monadMap_ExceptT_apply_eq`: empty description
- `Std.WP.WPMonad.wp_set_MonadStateOf_lift_apply_eq`: empty description
- `Std.WP.WPMonad.wp_tryCatch_lift_OptionT_apply_eq`: empty description
- `Std.TreeMap.empty`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeMap.getElem_inter`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.WP.Spec.tryCatch_MonadExcept`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SuperSymmetry.SU5.ChargeSpectrum.ofPotentialTerm'_mono`: empty description
- `Sym2.GameAdd.recursion_eq`: empty description
- `Tactic.ComputeAsymptotics.UnitMonomial.FirstNonzeroIsNeg.of_tail`: empty description
- `TensorSpecies.Tensor.IsReindexing.append_of_first`: empty description
- `TensorSpecies.Tensor.evalAdjustPos`: empty description
- `Topology.IsQuotientMap.isStrictMap_iff`: empty description
- `UnitAddTorus.coe_symm_measurableEquivPiIoc`: empty description
- `UInt16.ne_iff_toBitVec_ne`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ValueDistribution.Cartan.integrable_integral_norm_cartanKernel`: empty description
- `WeierstrassCurve.Jacobian.equiv_iff_eq_of_Z_eq'`: empty description
- `WeierstrassCurve.Projective.equiv_of_Z_eq_zero`: empty description
- `extChartAt_target_eventuallyEqSet_of_mem`: empty description
- `contMDiff_iff_comp_subtypeVal_Icc`: empty description
- `measurable_of_measurable_on_compl_countable`: empty description
- `AddConjClasses.quot_mk_eq_mk`: empty description
- `AddConjClasses.mk_surjective`: empty description
- `AlgebraicGeometry.Proj.mapAffineOpenCover_I₀`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Bundle.Trivialization.transFiberHomeomorph_apply`: empty description
- `CKMMatrix.cd_of_ud_us_ub_cb_tb`: empty description
- `Bool.xor_assoc`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Limits.Cone.equivalenceOfReindexing_functor`: empty description
- `CategoryTheory.Limits.Cone.functorialityEquivalence_counitIso`: empty description
- `CategoryTheory.MonoidalCategory.Arrow.PushoutProduct.whiskerRightIso`: empty description
- `CategoryTheory.ObjectProperty.instIsStableUnderShiftExtensionProductIterInt_1`: empty description
- `CategoryTheory.Sheaf.cohomologyFunctor`: empty description
- `CategoryTheory.ShortComplex.HomologyData.ofEpiMonoFactorisation.π_comp_isoHomology_hom_assoc`: empty description
- `CategoryTheory.Sieve.shrinkFunctorUliftFunctorIso_inv_ι_assoc`: empty description
- `CategoryTheory.Triangulated.TStructure.triangleLTLTGELT_obj_obj₁`: empty description
- `CategoryTheory.TwoSquare.isIso_ranBaseChange_app`: empty description
- `CategoryTheory.instFaithfulMonFunctorOppositeMonCatYonedaAddMon`: empty description
- `ClassicalMechanics.HarmonicOscillator.InitialConditions.trajectory_eq_cos`: empty description
- `ClassGroup.mulEquiv_symm_apply`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Classical.strongIndefiniteDescription`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ContinuousLinearMap.ofIsTopCompl_eq`: empty description
- `DirectSum.decomposeTensorEquiv_apply`: empty description
- `Equiv.xor_involutive`: empty description
- `FreeAddGroup.idxOf_toWord_add_of_eq`: empty description
- `Ideal.Factors.piQuotientEquiv_mk`: empty description
- `Int.Internal.Linear.Poly.denote'`: empty description
- `Int.testBit`: empty description
- `Lean.Grind.AC.diseq_norm_ai`: empty description
- `IsCoveringMap.coe_monodromyPerm`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `List.getLast_getLast_splitBy`: empty description
- `List.maxIdxOn_nil_eq_iff_false`: empty description
- `Measurable.fun_dite_const`: empty description
- `OrderType.type_congr`: empty description
- `Physlib.Fin.involutionNoFixedZeroSetEquivSetOne`: empty description
- `ProbabilityTheory.measurePreserving_eval_multivariateGaussian`: empty description
- `RootPairing.Base.cartanMatrix_mul_diagonal_eq`: empty description
- `RootPairing.Base.map_equiv_cartanMatrix`: empty description
- `QuantumMechanics.OneDimension.HarmonicOscillator.mul_power_integrable`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SimpleGraph.Walk.IsChordless.mem_edges`: empty description
- `Space.fderiv_normPowerSeries`: empty description
- `Std.DTreeMap.Internal.Impl.Const.get_filterMap`: empty description
- `Std.DTreeMap.Internal.Impl.getD_insert`: empty description
- `Std.DTreeMap.Const.isEmpty_filterMap_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.size_alter_eq_self_of_mem`: empty description
- `Std.DTreeMap.Internal.Impl.toList_keysArray`: empty description
- `Std.DTreeMap.Raw.Const.get!_erase_self`: empty description
- `Std.DTreeMap.Raw.Const.get!_diff_of_not_mem_left`: empty description
- `Std.DTreeMap.Internal.Impl.getKey!_erase_self`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Raw.Equiv.constGet?_eq`: empty description
- `Std.DTreeMap.Raw.contains_eq_isSome_getKey?`: empty description
- `Std.DTreeMap.Raw.foldlM_eq_foldlM_toList`: empty description
- `Std.DTreeMap.Raw.get?_alter`: empty description
- `Std.DTreeMap.Raw.getD_modify`: empty description
- `Std.DTreeMap.Raw.getKey?_eq_some_getKeyD_of_contains`: empty description
- `Std.DTreeMap.Raw.getKeyD_eq_of_contains`: empty description
- `Std.DTreeMap.Raw.size_filterMap_le_size`: empty description
- `Std.DTreeMap.Raw.map_fst_toList_eq_keys`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Raw.singleton_eq_insert`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.forM_eq_forM`: API error 429: This request would exceed your rate limit of 1,000 requests per minute (org: 914248f6-63be-4aab-9907-06a807d560e5, model: claude-haiku-5-5). For details, refer to: https://docs.claude.com/en/api/rate-limits. You can see the response headers for current usage. Try again later. View your current limits at https://console.anthropic.com/settings/limits. You may also contact sales at https://claude.com/contact-sales to discuss your options for a rate limit increase.
- `Std.Internal.List.getKey_getValue?_ext`: empty description
- `Std.Internal.List.minKey?_eq_head?_keys`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `UInt16.ofFin_le_iff_le`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `WickContraction.mem_of_mem_subContraction`: empty description
- `cfcₙHomTransfer_injective`: empty description
- `AlgebraicGeometry.QuasiCompactCover.uliftHom`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `BitVec.equivFin_apply`: empty description
- `Array.swapIfInBounds_def`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Batteries.BinaryHeap.insert_wf`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Batteries.BinaryHeap.size_insert`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Abelian.SpectralObject.δ_eq_zero_of_isIso₁`: empty description
- `CategoryTheory.CardinalDirectedPoset.coconeWithTop`: empty description
- `BitVec.ushiftRight_eq_zero`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Bool.ite_eq_false_distrib`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Limits.SequentialProduct.functorMap`: empty description
- `CategoryTheory.Abelian.SpectralObject.sc₂_X₃`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.RetractArrow.toLoc_id₁`: empty description
- `CategoryTheory.Triangulated.TStructure.liftTruncLT_ι_assoc`: empty description
- `ClassicalMechanics.DampedHarmonicOscillator.lagrangian_of_isUndamped`: empty description
- `ClassicalMechanics.PointParticle.NewtonianSystem.InternalForce.target`: empty description
- `CategoryTheory.MonoidalCategory.Arrow.PullbackHom.isTerminalIso_inv_left`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CochainComplex.mappingCocone.descCocycle_coe`: empty description
- `ContDiffMapSupportedIn.seminorm_monoLM_eq`: empty description
- `FTheory.SU5.ChargeSpectrum.allowsTerm_topYukawa_of_mem_viableCharges`: empty description
- `FTheory.SU5.TenQuanta.mem_liftCharge_of_mem_noExotics_hasNoZero`: empty description
- `FieldStatistic.ofFinset_eq_prod`: empty description
- `FreeAddGroup.idxOf_toWord_of_add_eq`: empty description
- `GradedAlgHom.restrictScalars_coe_linearMap`: empty description
- `GradedAlgHom.restrictScalars_injective`: empty description
- `HomotopyCategory.Plus.instInjectiveXIntAsHomologicalComplexUpHomotopicObjPlusObjPlusQuotientOfCochainComplexPlus`: empty description
- `FTheory.SU5.FluxesTen.card_le_three_of_noExotics`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Int.Internal.Linear.le_of_le`: empty description
- `GeneratedByTopCat.adjUnitIso`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `GalileanGroup.act`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lean.Grind.AC.diseq_simp_lhs_ac`: empty description
- `Lean.Grind.CommRing.cancel_var_cert`: empty description
- `IO.sleep`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `LinearOrderedCommGroup.closure_equiv_closure`: empty description
- `List.tail_append`: empty description
- `Mathlib.Tactic.Algebra.RingCompute.pow`: empty description
- `MvPolynomial.coeff_coeff_sumAlgEquiv_apply_apply_support_val`: empty description
- `Physlib.Wirtinger.dWirtingerCoord_comp_clm_zero`: empty description
- `MvPowerSeries.coeff_invOfUnit`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.fst_maxPowDvdDiv`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `RBTree.RBMap.mem_toList_insert`: empty description
- `Representation.finsuppTensorRight_symm_apply_single`: empty description
- `SSet.relativeCellComplexOfMono.Cell.ι_t_ι_eq_ι_l_b_ι`: empty description
- `SSet.horn.IsCompatible.exists_lift`: empty description
- `Space.IsDistBounded.inv_pow_smul_repr_self`: empty description
- `Space.radialAngularMeasure_zero_eq_volume`: empty description
- `StandardModel.HiggsVec.repGaugeGroupI_inv_apply_eq_iff`: empty description
- `Std.DTreeMap.Const.compare_maxKey_modify_eq`: empty description
- `Std.DTreeMap.Internal.Impl.Const.contains_alter`: empty description
- `Std.DTreeMap.Internal.Impl.maxKey_erase_eq_iff_not_compare_eq_maxKey`: empty description
- `Std.DTreeMap.Internal.Impl.Const.getD_insert`: empty description
- `Std.DTreeMap.Internal.Impl.minKey?_insert!`: empty description
- `Std.DTreeMap.Raw.Const.contains_alter`: empty description
- `Std.DTreeMap.Raw.Const.get!_insertMany_list`: empty description
- `Std.DTreeMap.Raw.Const.get!_modify`: empty description
- `Std.DTreeMap.Raw.Const.getD_map'`: empty description
- `Std.DTreeMap.Raw.Const.getKey?_modify`: empty description
- `Std.DTreeMap.Raw.getKey?_alter_self`: empty description
- `Std.DTreeMap.Internal.Impl.keys_filter!`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Raw.get_insertMany_list_of_contains_eq_false`: empty description
- `Std.DTreeMap.get!_alter`: empty description
- `Std.HashMap.getElem?_of_isEmpty`: empty description
- `Std.HashSet.Raw.isEmpty_inter_left`: empty description
- `Std.DTreeMap.get_getKey?`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Tactic.BVDecide.BVExpr.bitblast_lt_size_of_lt_aig_size`: empty description
- `Std.Iter.length_attachWith`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Iterators.HetT.prun`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeMap.Raw.compare_minKeyD_modify_eq`: empty description
- `Std.TreeMap.Raw.getElem_insertMany_list_of_mem`: empty description
- `Std.TreeMap.Raw.getKey!_diff_of_mem_right`: empty description
- `Std.Time.PlainDate.toAmericanDateString`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeMap.Raw.getKey_union_of_not_mem_left`: empty description
- `Std.TreeMap.Raw.insertMany_cons`: empty description
- `Std.TreeMap.Raw.getKeyD_insertMany_list_of_mem`: empty description
- `Std.TreeMap.Raw.minKeyD_insert_le_minKeyD`: empty description
- `UInt8.toNat_neg`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `USize.add_def`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `groupCohomology.cochainsMap_congr`: empty description
- `ite_congr`: empty description
- `lorentzAlgebra.rotationGenerator`: empty description
- `Array.foldl_rel`: empty description
- `AffineSubspace.dim_bot`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `AffineSubspace.dim_toAffineSubspace`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `AffineSubspace.dim_map_of_injective`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `AffineSubspace.dim_mono`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `AddSubgroup.instIsAddCommutativeQuotientSubtypeMemFocalAddSubgroupOf`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `AffineSubspace.dim_map_le`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `ByteArray.extract_append_size_left`: empty description
- `CategoryTheory.Abelian.SpectralObject.composableArrows₅_exact`: empty description
- `CategoryTheory.AddMon.EquivLaxMonoidalFunctorPUnit.addUnitIso_inv_app_hom_app`: empty description
- `Fermion.leftLeftToMatrix`: empty description
- `Finset.intervalGapsWithin_fst_le_snd`: empty description
- `ISize.ofBitVec_ofNatLT`: empty description
- `IdealGas.ZIntegrable`: empty description
- `Int64.ofNat_le_iff_le`: empty description
- `Lean.Order.ofProp_meet_le_right`: empty description
- `List.Perm.idxBij_symm_rightInverse_idxBij`: empty description
- `Lean.Grind.CommRing.Stepwise.inv`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lean.Grind.Ring.OfSemiring.pow_zero`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Mathlib.Tactic.Linarith.mk_toReal_nonneg_prf`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SSet.stdSimplex.objMk₁_surjective`: empty description
- `ShareCommonT.run`: empty description
- `Rat.ofScientific`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Representation.LinearizeMonoidal.δ_μ`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.Const.getKey?_insertMany_list_of_mem`: empty description
- `Std.DTreeMap.Internal.Impl.Const.size_insertMany_empty_list_le`: empty description
- `Std.DHashMap.Raw.size_map`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.getKey!_diff!`: empty description
- `Std.DTreeMap.Internal.Impl.Const.get!_eq_get!ₘ`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.maxKey?_insertIfNew`: empty description
- `Std.DTreeMap.Raw.Const.get!_filter_of_getKey?_eq_some`: empty description
- `Std.DTreeMap.Raw.minKey!_alter_eq_self`: empty description
- `Std.DTreeMap.Raw.maxKey!_insert`: empty description
- `Std.Do.WP.adapt_ReaderT`: empty description
- `Std.Do.WP.monadMap_trans`: empty description
- `Std.DTreeMap.Internal.Impl.self_le_maxKey!_insertIfNew`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Do.WP.throwThe`: empty description
- `Std.HashSet.isEmpty_iff_forall_not_mem`: empty description
- `Std.Do.WP.withReader_MonadWithReader`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Internal.List.getValueCast!_eraseKey`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeMap.Raw.getKey!_filter`: empty description
- `Std.Sat.AIG.RefVec.map`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Sat.CNF.Clause.literals_append`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeSet.Raw.getD_union_of_not_mem_left`: empty description
- `Std.TreeMap.getKey_maxKey`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeMap.getKey_filter`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeSet.Equiv.toList_eq`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeSet.get?_maxD`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.max_eq_of_maxOfLe`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `TopPair.HomologyPretheory.Hom.iso_comm_app`: empty description
- `String.Slice.Pos.le_ofSliceTo_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `TwoHiggsDoublet.PotentialIsStable`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Vector.mapIdxM_mk`: empty description
- `USize.toBitVec32_max`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `USize.toBitVec32_mod`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `groupCohomology.cocyclesMap_cocyclesIso₀_hom_f_apply`: empty description
- `Array.le_max_of_mem`: empty description
- `Batteries.UnionFind.parent_link`: empty description
- `Bundle.Trivialization.coe_symmₗ`: empty description
- `Bundle.Trivialization.pullback_apply`: empty description
- `ByteArray.getElem_set!_self`: empty description
- `CategoryTheory.Abelian.Preradical.isLimitKernelForkObj`: empty description
- `CartanMatrix.Realisation.pairing_fst_snd`: empty description
- `BitVec.neg_slt_zero`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.MonoidalCategory.Arrow.PushoutProduct.isInitialIsTerminalIso'_inv_right`: empty description
- `CanonicalEnsemble.mathematicalPartitionFunction_add`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Abelian.SpectralObject.sc₁_X₁`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Fermion.Dirac.gammaMatrix`: empty description
- `CategoryTheory.MonoidalCategory.Arrow.PushoutProduct.hexagon_forward`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.MonoidalCategory.Arrow.PushoutProduct.hexagon_reverse`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `FiberBundle.contMDiffAt_extend'`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Lorentz.coBasis_toFin1dℝ`: empty description
- `Lorentz.contrModCoModBi`: empty description
- `Nat.stepInduction`: empty description
- `Lorentz.ContrMod.toSelfAdjoint_apply_coe`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `SSet.Subcomplex.Pairing.RankFunction.Cell.ι_m`: empty description
- `SheafOfModules.Presentation.mapGenerators`: empty description
- `QuantumMechanics.HydrogenAtom.wavelength`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Space.distOfFunction_smul_fun`: empty description
- `Rat.ofScientific_true_def`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.Const.getD_inter!_of_contains_right`: empty description
- `Std.DTreeMap.Internal.Impl.Const.getD_inter_of_contains_eq_false_left`: empty description
- `Std.DTreeMap.Raw.Const.get!_insertIfNew`: empty description
- `Std.DTreeMap.Internal.Impl.contains_inter`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.get?_diff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.get!_inter_of_contains_right`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Do.Spec.restoreM_refl`: empty description
- `Std.Do.Spec.tryCatch_OptionT_lift`: empty description
- `Std.Do.Spec.IterM.fold_map`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.ExtDTreeMap.getKey!_alter`: empty description
- `Std.HashMap.Raw.Equiv.map`: empty description
- `Std.TreeMap.Raw.maxKey!_erase_le_maxKey!`: empty description
- `Std.TreeMap.Raw.minKey!_modify`: empty description
- `Std.TreeMap.Raw.maxKey!_insert`: empty description
- `Std.TreeSet.Raw.foldl_eq_foldl_toArray`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `TensorSpecies.Tensor.Pure.prodP_permP_right`: empty description
- `TensorSpecies.Tensor.evalT_evalT_basis`: empty description
- `sum_generalizedKroneckerDelta_mul_snoc`: empty description
- `CategoryTheory.Functor.whiskeringLeft₄_obj_obj_obj_obj_obj_obj_obj_map_app`: empty description
- `BitVec.bit_neg_eq_neg`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Triangulated.TStructure.triangleLEGT_map_hom₂`: empty description
- `CategoryTheory.Abelian.SpectralObject.SpectralSequence.pageXIso`: empty description
- `BitVec.toNat_sdiv`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Electromagnetism.TempDistElectromagneticPotential.threeDimPointParticleCurrentDensity_eq_distTranslate`: empty description
- `Fermion.Dirac.gamma`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `FieldSpecification.FieldOpFreeAlgebra.universality`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `IO.FS.Handle.readToEnd`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `HomotopicalAlgebra.CofibrantObject.bifibrantResolutionMap_fac'_assoc`: empty description
- `Int32.toInt32_toISize`: empty description
- `Int32.toNat_toBitVec_of_le`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.strongRec_eq`: empty description
- `QuantumMechanics.radiusRegPowOperator_hasDenseDomain`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.Const.getKey!_insertManyIfNewUnit!_list_of_not_mem_of_contains_eq_false`: empty description
- `Std.DTreeMap.Internal.Impl.Const.getKey!_insertManyIfNewUnit!_list_of_not_mem_of_mem`: empty description
- `Std.DTreeMap.Internal.Impl.Const.size_insertManyIfNewUnit_list_le`: empty description
- `Std.DTreeMap.Raw.Const.get!_ofList_of_mem`: empty description
- `SheafOfModules.Presentation.mapRelations_mapGenerators`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.Internal.Impl.Const.getKey?_insertManyIfNewUnit_list_of_mem`: empty description
- `Std.DTreeMap.Raw.Const.getD_ofList_of_mem`: empty description
- `StandardModel.HiggsField.Potential.IsBounded`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DTreeMap.get!_ofList_of_mem`: empty description
- `Std.DTreeMap.get_insertMany_list_of_contains_eq_false`: empty description
- `Std.ExtDTreeMap.Const.compare_minKey?_modify_eq`: empty description
- `Std.ExtDTreeMap.get?_insertMany_list_of_mem`: empty description
- `Std.DTreeMap.Internal.Impl.Const.getD_insertManyIfNewUnit_list`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.ExtDTreeMap.minKey!_le_of_contains`: empty description
- `Std.HashSet.emptyWithCapacity_equiv_iff_isEmpty`: empty description
- `Std.Sat.CNF.Clause.not_unsat_unit`: empty description
- `Std.ExtTreeMap.minKeyD_erase_eq_of_not_compare_minKeyD_eq`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeSet.Raw.le_max!_of_mem`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `UInt32.toUInt16_lt`: empty description
- `UInt32.toUInt8_ofNatClamp_of_lt`: empty description
- `Tactic.ComputeAsymptotics.MultiseriesExpansion.monomialRpow_approximates`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `WickContraction.getDual?_get_self_mem`: empty description
- `ZeroAtInftyContinuousMap.toAlgEquiv_unitizationStarAlgEquiv`: empty description
- `WickContraction.signInsertNone_eq_filter_map`: empty description
- `CategoryTheory.Abelian.SpectralObject.kernelSequenceOpcycles_X₃`: empty description
- `CategoryTheory.GrothendieckTopology.Point.sheafFiberMapIso`: empty description
- `ArithmeticFunction.dirichletInverse`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Array.count_attachWith`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Array.count_empty`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Int.Internal.Linear.dvd_solve_combine`: empty description
- `Int64.ofIntLE_eq_ofIntTruncate`: empty description
- `HomotopicalAlgebra.FibrantObject.HoCat.resolutionCompToLocalizationNatTrans`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `PureU1.VectorLikeEvenPlane.Shifted.basis_linear_independent`: empty description
- `SSet.prodStdSimplex.pairing_castSucc`: empty description
- `SSet.prodStdSimplex.pairingCore.isIndex_zero`: empty description
- `SSet.prodStdSimplex.pairingCore.isIndex_succ`: empty description
- `Std.DHashMap.Internal.Raw₀.Const.foldRev_eq_foldr_toList`: empty description
- `Std.DHashMap.Raw.keys_map`: empty description
- `SMRHN.SMNoGrav_quadraticACCs`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.Raw.isEmpty_toList`: empty description
- `Std.DTreeMap.Internal.Impl.get!_union!`: empty description
- `Std.DTreeMap.Internal.Impl.get?_union`: empty description
- `Std.DTreeMap.Internal.Impl.getKey!_union!`: empty description
- `Std.DTreeMap.Internal.Impl.getKeyD_union!_of_contains_eq_false_right`: empty description
- `Std.DTreeMap.Raw.Const.get!_insertManyIfNewUnit_list`: empty description
- `Std.ExtDTreeMap.Const.getKeyD_filter`: empty description
- `Std.ExtTreeMap.getKeyD_insertMany_list_of_mem`: empty description
- `Std.ExtTreeMap.mem_of_mem_insertIfNew'`: empty description
- `Std.ExtDTreeMap.maxKey_erase_eq_iff_not_compare_eq_maxKey`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeMap.Raw.getElem_ofList_of_mem`: empty description
- `Std.ExtTreeSet.size_inter_eq_size_right`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Http.Request.Builder.fromBytes`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `String.Slice.Pattern.Model.eq_append_of_dropPrefix?_eq_some`: empty description
- `Std.Tactic.BVDecide.BVLogicalExpr.sat_and`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `String.Slice.getUTF8Byte_copy`: empty description
- `String.posLE_eq_pos`: empty description
- `TensorSpecies.Tensor.fromPairT_tmul`: empty description
- `String.Slice.endsWith_prop_eq_getLast?`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Array.takeWhile_eq_extract_findIdx_not`: empty description
- `CategoryTheory.Functor.OneHypercoverDenseData.essSurj.presheafMap_comp_assoc`: empty description
- `FieldSpecification.FieldOpFreeAlgebra.summerCommute_jacobi_ofCrAnListF`: empty description
- `Int.toArray_roc_succ_succ`: empty description
- `List.toArray_mkSlice_roo`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `List.utf8DecodeChar_utf8Encode_cons`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `PureU1.VectorLikeEvenPlane.Pa_zero`: empty description
- `SSet.prodStdSimplex.pairingCore.IsType₂.simplex`: empty description
- `Nat.toArray_rco_eq_empty`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.toArray_roo_eq_push`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.Internal.Raw₀.toListModel_containsThenInsertIfNew`: empty description
- `Std.DHashMap.Internal.Raw₀.Const.wfImp_modify`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.forM_eq_forM_keys`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashMap.Raw.all_keys`: empty description
- `Std.Ric.toArray_eq_ite_roc`: empty description
- `Std.Ric.toArray_ne_empty`: empty description
- `Std.ExtTreeSet.le_max!_of_mem`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeMap.Raw.getKey!_insertManyIfNewUnit_list_of_mem`: empty description
- `Std.TreeMap.Raw.size_insertManyIfNewUnit_list`: empty description
- `Std.Time.PlainDateTime.weekOfMonth`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `String.posLE_dec`: empty description
- `String.posLT_eq_posLE`: empty description
- `FieldSpecification.FieldOpFreeAlgebra.fermionicProjF_ofCrAnListF_if_bosonic`: empty description
- `CategoryTheory.SimplicialObject.Splitting.toKaroubiNondegComplexIsoN₁_hom_inv_id_f`: empty description
- `Array.contains_empty`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Array.all_eq_false'`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Array.all_takeWhile`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Int.toList_rco_append_toList_rco`: empty description
- `Int.toList_roo_eq_ite`: empty description
- `Int.getElem_toList_rcc`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Nat.getElem?_toList_ric_eq_some_iff`: empty description
- `MeasureTheory.VectorMeasure.integral_eq_setToFun_transpose`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `PureU1.VectorLikeEvenPlane.Shifted.swap_as_add`: empty description
- `QuantumMechanics.angularMomentumLoweringOperator`: empty description
- `SMRHN.SM.PlaneSeven.B₂_Bi_cubic`: empty description
- `Nat.map_add_toList_roo'`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.Const.getKey?_modify_self`: empty description
- `Std.DHashMap.Internal.Raw₀.Const.get!_insertMany_emptyWithCapacity_list_of_mem`: empty description
- `Std.DHashMap.Internal.Raw₀.Const.getKey!_insertMany_emptyWithCapacity_list_of_mem`: empty description
- `Std.DHashMap.Raw.Const.get!_eq_default_of_contains_eq_false`: empty description
- `Std.DHashMap.Internal.Raw₀.Const.getThenInsertIfNew?_eq_get?ₘ`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.Raw.Const.getD_filterMap_of_getKey?_eq_some`: empty description
- `Std.DHashMap.Raw.Const.getD_of_isEmpty`: empty description
- `Std.DHashMap.Raw.Equiv.getKey!_eq`: empty description
- `Std.DHashMap.Raw.contains_of_contains_filterMap`: empty description
- `Std.DHashMap.Raw.get!_diff_of_not_mem_right`: empty description
- `Std.DHashMap.Raw.get?_insert`: empty description
- `Std.DHashMap.Raw.getKey!_inter_of_not_mem_left`: empty description
- `Std.DHashMap.Raw.getKey!_inter_of_mem_right`: empty description
- `Std.DHashMap.Raw.getKey!_inter_of_not_mem_right`: empty description
- `Std.DHashMap.Raw.getKey?_congr`: empty description
- `Std.DHashMap.Raw.getKey?_eq_some_getKey!`: empty description
- `Std.DHashMap.Raw.getKey?_inter_of_not_mem_right`: empty description
- `Std.DHashMap.Raw.Const.getD_eq_fallback_of_contains_eq_false`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.Raw.getKeyD_of_isEmpty`: empty description
- `Std.DHashMap.Raw.Const.mem_alter_self`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.get?_emptyWithCapacity`: empty description
- `Std.DHashMap.get?_union_of_not_mem_left`: empty description
- `Std.DHashMap.Raw.getKeyD_modify`: stopped: max_tokens
- `Std.DHashMap.getKey!_eq_default_of_contains_eq_false`: empty description
- `Std.DHashMap.getKey!_erase`: empty description
- `Std.DHashMap.getKeyD_map`: empty description
- `Std.DHashMap.size_alter`: empty description
- `Std.DHashMap.union_insert_right_equiv_insert_union`: empty description
- `Std.Do.Spec.forIn'_rii`: empty description
- `Std.HashSet.Raw.mem_of_mem_toList`: empty description
- `Std.HashSet.Raw.Equiv.toList_perm`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.Http.URI.all_of_all_of_imp`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.TreeSet.Raw.getD_insertMany_list_of_mem`: empty description
- `String.Legacy.Iterator.Valid.remainingToString`: empty description
- `String.Pos.Splits.of_next`: empty description
- `Std.Rco.toList_iter_eq_toList`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `String.Pos.prev_ofSliceTo`: empty description
- `Subarray.toList_mkSlice_roi`: empty description
- `TensorSpecies.unitTensor_congr`: empty description
- `WickContraction.insertLift_none_surjective`: empty description
- `TensorSpecies.Tensor.crossToEnd_dual_metricTensor_metricTensor`: empty description
- `CategoryTheory.Functor.OneHypercoverDenseData.essSurj.presheafObjObjIso.hom_mapPreimage`: empty description
- `Lorentz.CoVector.toTensor_symm_basis`: empty description
- `Std.DHashMap.Const.getKeyD_insertManyIfNewUnit_list_of_not_mem_of_mem`: empty description
- `Std.DHashMap.Raw.Const.Equiv.beq_congr`: empty description
- `Std.DHashMap.Raw.Const.any_eq_false_iff_forall_mem_get`: empty description
- `Std.DHashMap.Raw.Const.get_insert_self`: empty description
- `Std.DHashMap.Const.unitOfArray`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.Raw.getKey_insertMany_list_of_mem`: empty description
- `Std.DHashMap.Raw.getKey_eq_get_getKey?`: empty description
- `Std.DHashMap.Raw.get_inter`: empty description
- `Std.DHashMap.Raw.size_filterMap_eq_size_iff`: empty description
- `Std.DHashMap.getKeyD_filterMap`: empty description
- `Std.DHashMap.get_eq_get!`: empty description
- `Std.DHashMap.size_filter_key_eq_size_iff`: empty description
- `Std.DHashMap.Raw.Const.get_insertIfNew`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashMap.Raw.getElem!_modify`: empty description
- `Std.HashMap.Raw.getElem?_eq_some_getD_of_contains`: empty description
- `Std.DHashMap.Raw.mem_of_get_eq`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashMap.Raw.getKey?_alter`: empty description
- `Std.HashMap.Raw.getKeyD_erase_self`: empty description
- `Std.ExtDHashMap.Const.get!`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashMap.Raw.mem_of_getKey?_eq_some`: empty description
- `Std.HashMap.getD_map_of_getKey?_eq_some`: empty description
- `Std.HashMap.getElem!_eq_default_of_contains_eq_false`: empty description
- `Std.HashMap.getElem?_filterMap_of_getKey?_eq_some`: empty description
- `Std.HashMap.getKeyD_insertIfNew`: empty description
- `Std.HashMap.isEmpty_insert`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `String.Slice.find?_bool_eq_some_iff`: empty description
- `Electromagnetism.ElectromagneticPotential.toFieldStrength_eq_tensorDeriv`: empty description
- `FieldSpecification.WickAlgebra.ι_eq_zero_iff_ι_bosonicProjF_fermonicProj_zero`: empty description
- `MSSMACC.B₃AsCharge`: empty description
- `Std.DHashMap.Const.getD_diff_of_mem_right`: empty description
- `Std.DHashMap.Const.getKey!_ofList_of_mem`: empty description
- `Std.DHashMap.Const.getD_ofList_of_mem`: empty description
- `Std.DHashMap.Const.get?_diff_of_not_mem_right`: empty description
- `SSet.PInfty_toNormalizedChainComplex_assoc`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.DHashMap.Const.getKey_ofList_of_mem`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.ExtDHashMap.Const.getKey!_insertMany_list_of_mem`: empty description
- `Std.ExtDHashMap.getKey!_erase`: empty description
- `Std.ExtDHashMap.contains_empty`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashMap.Raw.all_eq_true_iff_forall_mem_getKey_getElem`: empty description
- `Std.HashMap.Raw.getElem?_insertManyIfNewUnit_list`: empty description
- `Std.HashMap.Raw.getElem_insertMany_list_of_mem`: empty description
- `Std.HashMap.Raw.getKey_beq`: empty description
- `Std.ExtDHashMap.getKey?_eq_some_of_contains`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashMap.getKeyD_filterMap`: empty description
- `Std.HashMap.insertManyIfNewUnit_cons`: empty description
- `Std.HashMap.mem_insertMany_of_mem`: empty description
- `Std.HashSet.Raw.contains_of_contains_insert'`: empty description
- `Std.HashSet.Raw.get!_diff_of_not_mem_left`: empty description
- `Std.HashSet.Raw.get!_inter_of_not_mem_left`: empty description
- `Std.HashSet.Raw.get?_diff`: empty description
- `Std.HashSet.Raw.get?_erase_self`: empty description
- `Std.HashSet.Raw.getD_union_of_not_mem_right`: empty description
- `Std.HashMap.Raw.getKey_modify_self`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashSet.getD_insert`: empty description
- `String.Pos.find?_char_eq_none_iff`: empty description
- `WickContraction.uncontractedListEmd_toFun_eq_get`: empty description
- `PauliMatrix.auliContrDown_pauliContr_mul_add`: empty description
- `PauliMatrix.pauliContr_mul_pauliContrDown_add`: empty description
- `Std.ExtDHashMap.Const.get_ofList_of_mem`: empty description
- `CategoryTheory.Abelian.SpectralObject.kernelSequenceE_g`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Abelian.SpectralObject.opcyclesMap_threeδ₂Toδ₁_opcyclesToE`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Abelian.SpectralObject.kernelSequenceE_X₂`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Abelian.SpectralObject.kernelSequenceE_f`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `CategoryTheory.Abelian.SpectralObject.opcyclesToE_ιE_assoc`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.ExtDHashMap.ofList_eq_foldl`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.ExtHashMap.filter_eq_self_iff`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashMap.getElem_insertMany_list_of_contains_eq_false`: empty description
- `Std.HashMap.getKey?_unitOfList_of_contains_eq_false`: empty description
- `Std.HashMap.getKey?_ofList_of_mem`: empty description
- `Std.HashSet.Raw.get?_insertMany_list_of_mem`: empty description
- `Std.HashSet.Raw.getD_insertMany_list_of_not_mem_of_contains_eq_false`: empty description
- `Std.HashSet.insertMany_append`: empty description
- `Electromagnetism.TempDistElectromagneticPotential.isExtrema_iff_space_time`: empty description
- `CategoryTheory.Abelian.SpectralObject.toCycles_πE_descE_assoc`: API error 429: Number of concurrent requests across all models has exceeded your organization's limit. Wait for in-flight requests to complete and retry. Limits increase automatically as your organization moves up usage tiers.
- `Std.HashSet.get!_ofList_of_mem`: empty description
- `CategoryTheory.Abelian.SpectralObject.shortComplexOpcyclesThreeδ₂Toδ₁_X₁`: empty description
- `MSSMACC.lineY₃B₃Charges_quad`: empty description
- `CategoryTheory.Abelian.SpectralObject.dKernelSequence_X₃`: empty description
- `FieldSpecification.WickAlgebra.ofFieldOpList_mul_ofFieldOpList_eq_superCommute`: empty description
- `MSSMACC.AnomalyFreePerp.inQuadCubeProj`: empty description

## Checks

| Check | Passed |
|---|---:|
| LaTeX balanced and KaTeX-compatible | 122295/126732 (96%) |
| Cited Lean names are used by the declaration | 119939/126732 (95%) |
| No cited Lean name is unknown | 126413/126732 (100%) |
| All checks | 115432/126732 (91%) |
| Has a LaTeX formula | 88464/126732 (70%) |

Failing checks (first 15):

- `ACCSystemCharges`: cites `ACCSystemCharges.Charges`, which it doesn't use
- `AEMeasurable.fun_vadd`: cites `AEMeasurable.fun_smul`, which it doesn't use
- `ACCSystemLinear`: cites `ACCSystemLinear.LinSols`, which it doesn't use
- `AddSubgroup.addCommutator_characteristic`: \llbracket isn't a KaTeX command; \rrbracket isn't a KaTeX command; cites `Subgroup.commutator_characteristic`, which it doesn't use
- `AddOpposite.isUniformEmbedding_unop`: cites `MulOpposite`, which it doesn't use
- `AddSubgroup.addCommutator_normal`: \llbracket isn't a KaTeX command; \rrbracket isn't a KaTeX command
- `AlgHom.domRestrict`: \algebraMap isn't a KaTeX command
- `AlgebraicGeometry.Scheme.PartialIso`: cites `AlgebraicGeometry.Scheme.Birational`, which it doesn't use
- `CategoryTheory.Abelian.AbelianStruct`: cites `CategoryTheory.Abelian.mk'`, which it doesn't use
- `CategoryTheory.AddMonObj`: cites `CategoryTheory.MonObj`, which it doesn't use
- `CategoryTheory.AddMon`: cites `CategoryTheory.Mon`, which it doesn't use
- `AlgebraicGeometry.Scheme.PartialMap.id`: \dashrightarrow isn't a KaTeX command
- `CategoryTheory.Bicategory.Adjunction.compCounit`: cites `Bicategory.Adjunction.comp`, which it doesn't use
- `CategoryTheory.Bicategory.Equivalence.adj`: \inv isn't a KaTeX command
- `CategoryTheory.CommRingObjCat`: cites `CategoryTheory.RingObjCat`, which it doesn't use

## Samples (40)

### `AlgebraicGeometry.Scheme.ProEt.forget_obj`

theorem in `Mathlib.AlgebraicGeometry.Sites.Proetale`

```lean
AlgebraicGeometry.Scheme.ProEt.forget_obj.{u} (S : AlgebraicGeometry.Scheme)
  (X :
    CategoryTheory.MorphismProperty.Comma (CategoryTheory.Functor.id AlgebraicGeometry.Scheme)
      (CategoryTheory.Functor.fromPUnit S) @AlgebraicGeometry.WeaklyEtale ⊤ ⊤) :
  (AlgebraicGeometry.Scheme.ProEt.forget S).obj X = X.toComma
```

**Underlying over-object of a weakly étale S-scheme in the pro-étale site**. For a scheme $S$ and an object $X$ of the small pro-étale site of $S$, i.e. a weakly étale $S$-scheme, the forgetful functor `AlgebraicGeometry.Scheme.ProEt.forget` sends $X$ to its underlying object $X.\mathrm{toComma}$ in the over category of $S$, that is, the structure morphism to $S$ with the weakly étale condition forgotten. This is a definitional unfolding of the object part of `AlgebraicGeometry.Scheme.ProEt.forget`.

$$
\mathrm{forget}_S(X) = X.\mathrm{toComma}
$$

Queries: “what does the forgetful functor from the small pro-étale site of a scheme do on objects” · “forget_obj pro-étale site weakly étale S-scheme over category” · “ProEt forget obj toComma weakly étale scheme structure morphism”

### `CategoryTheory.Adjunction.equivHomsetRightOfNatIso_apply`

theorem in `Mathlib.CategoryTheory.Adjunction.Basic`

```lean
CategoryTheory.Adjunction.equivHomsetRightOfNatIso_apply.{v₁, v₂, u₁, u₂} {C : Type u₁}
  [CategoryTheory.Category.{v₁, u₁} C] {D : Type u₂} [CategoryTheory.Category.{v₂, u₂} D]
  {F F' : CategoryTheory.Functor C D} (iso : F ≅ F') {X : C} {Y : D} (f : Y ⟶ F.obj X) :
  (CategoryTheory.Adjunction.equivHomsetRightOfNatIso iso) f = CategoryTheory.CategoryStruct.comp f (iso.hom.app X)
```

**Hom-set equivalence from a natural isomorphism of functors, applied to a morphism**. For a natural isomorphism $\iota : F \cong F'$ between functors $F, F' : C \to D$, the hom-set bijection `CategoryTheory.Adjunction.equivHomsetRightOfNatIso` sends a morphism $f : Y \to F(X)$ to the composite $f \circ \iota_X : Y \to F'(X)$, where $\iota_X$ is the component of $\iota$ at $X$. This is the defining computation of that equivalence.

$$
\mathrm{equivHomsetRightOfNatIso}(\iota)(f) = f \circ \iota_X
$$

Queries: “how does a natural isomorphism of functors induce a bijection between morphisms Y → F X and Y → F' X” · “f ≫ iso.hom.app X equivHomsetRightOfNatIso apply” · “natural isomorphism hom set equivalence composition with component iso.hom.app”

### `CategoryTheory.GrothendieckTopology.Point.skyscraperSheafFunctor_map_hom`

theorem in `Mathlib.CategoryTheory.Sites.Point.Skyscraper`

```lean
CategoryTheory.GrothendieckTopology.Point.skyscraperSheafFunctor_map_hom.{w, v', v, u', u} {C : Type u}
  [CategoryTheory.Category.{v, u} C] {J : CategoryTheory.GrothendieckTopology C} (Φ : J.Point) {A : Type u'}
  [CategoryTheory.Category.{v', u'} A] [CategoryTheory.Limits.HasProducts A] {X✝ Y✝ : A} (f : X✝ ⟶ Y✝) :
  (Φ.skyscraperSheafFunctor.map f).hom = Φ.skyscraperPresheafFunctor.map f
```

**Underlying morphism of the skyscraper sheaf functor on morphisms**. For a point $\Phi$ of a site $(C, J)$ and a category $A$ with all products, the underlying presheaf morphism of the skyscraper sheaf functor applied to a morphism $f$ in $A$ is the morphism given by the skyscraper presheaf functor applied to $f$. In other words, the skyscraper sheaf functor and the skyscraper presheaf functor agree on morphisms after forgetting the sheaf structure.

$$
(\Phi.\mathrm{skyscraperSheafFunctor}.\mathrm{map}\, f).\mathrm{hom} = \Phi.\mathrm{skyscraperPresheafFunctor}.\mathrm{map}\, f
$$

Queries: “skyscraper sheaf functor map underlying presheaf morphism equals skyscraper presheaf functor map at a point of a site” · “Φ.skyscraperSheafFunctor.map f .hom = Φ.skyscraperPresheafFunctor.map f” · “skyscraper sheaf functor morphism point Grothendieck topology presheaf hom”

### `CategoryTheory.MonoidalCategory.DayFunctor.equiv_inverse_obj_functor`

theorem in `Mathlib.CategoryTheory.Monoidal.DayConvolution.DayFunctor`

```lean
CategoryTheory.MonoidalCategory.DayFunctor.equiv_inverse_obj_functor.{v₁, v₂, u₁, u₂} (C : Type u₁)
  [CategoryTheory.Category.{v₁, u₁} C] (V : Type u₂) [CategoryTheory.Category.{v₂, u₂} V]
  [CategoryTheory.MonoidalCategory C] [CategoryTheory.MonoidalCategory V] (F : CategoryTheory.Functor C V) :
  ((CategoryTheory.MonoidalCategory.DayFunctor.equiv C V).inverse.obj F).functor = F
```

**Inverse of the tautological Day functor equivalence on objects**. For monoidal categories $C$ and $V$ and a functor $F : C \to V$, the underlying functor of the object $F$ viewed as a Day functor via the inverse of the tautological equivalence `CategoryTheory.MonoidalCategory.DayFunctor.equiv` is $F$ itself. In other words, the equivalence between ordinary functors $C \Rightarrow V$ and Day functors acts as the identity on underlying functors.

$$
\left(\mathrm{equiv}^{-1}(F)\right).\mathrm{functor} = F
$$

Queries: “Day functor equivalence inverse object functor is the identity” · “\mathrm{equiv}^{-1}(F).\mathrm{functor} = F Day convolution” · “Day functor equiv inverse obj functor monoidal category functor identity”

### `CategoryTheory.MorphismProperty.Comma.mapRightIso_inverse_obj_right`

theorem in `Mathlib.CategoryTheory.MorphismProperty.Comma`

```lean
CategoryTheory.MorphismProperty.Comma.mapRightIso_inverse_obj_right.{v_1, u_1, v_2, u_2, v_3, u_3} {A : Type u_1}
  [CategoryTheory.Category.{v_1, u_1} A] {B : Type u_2} [CategoryTheory.Category.{v_2, u_2} B] {T : Type u_3}
  [CategoryTheory.Category.{v_3, u_3} T] (L : CategoryTheory.Functor A T) {P : CategoryTheory.MorphismProperty T}
  {Q : CategoryTheory.MorphismProperty A} {W : CategoryTheory.MorphismProperty B} [Q.IsMultiplicative]
  [W.IsMultiplicative] {R₁ R₂ : CategoryTheory.Functor B T} [P.RespectsIso] [Q.RespectsIso] [W.RespectsIso]
  (e : R₁ ≅ R₂) (X : CategoryTheory.MorphismProperty.Comma L R₂ P Q W) :
  ((CategoryTheory.MorphismProperty.Comma.mapRightIso L e).inverse.obj X).right = X.right
```

**Right component of the inverse of the right-isomorphism equivalence on comma subcategories**. For an isomorphism $e : R_1 \cong R_2$ of functors $B \to T$, the inverse of the equivalence `CategoryTheory.MorphismProperty.Comma.mapRightIso` between comma subcategories sends an object $X$ of `P.Comma L R₂ Q W` to an object whose right component is the same as that of $X$. The equivalence only changes the structural morphism, so the right object is unchanged.

$$
\left(\mathrm{inverse}(X)\right).\mathrm{right} = X.\mathrm{right}
$$

Queries: “inverse of mapRightIso equivalence preserves the right object of a comma subcategory object” · “comma category morphism property equivalence induced by natural isomorphism of right functors, right component unchanged” · “MorphismProperty.Comma mapRightIso inverse obj right”

### `CategoryTheory.Subobject.supMonoFactorisation_m`

theorem in `Mathlib.CategoryTheory.Subobject.Lattice`

```lean
CategoryTheory.Subobject.supMonoFactorisation_m.{v₁, u₁} {C : Type u₁} [CategoryTheory.Category.{v₁, u₁} C]
  [CategoryTheory.Limits.HasImages C] [CategoryTheory.Limits.HasBinaryCoproducts C] {A : C}
  (f g : CategoryTheory.Subobject A) : (f.supMonoFactorisation g).m = (f ⊔ g).arrow
```

**Mono factorisation of the coproduct map has image the join of subobjects**. For subobjects $f$ and $g$ of an object $A$ in a category with images and binary coproducts, the monomorphism $m$ in the mono factorisation of the coproduct map $f \amalg g \to A$ given by `CategoryTheory.Subobject.supMonoFactorisation` is equal to the arrow of the join $f \sqcup g$. So the factorisation's middle monomorphism is the defining arrow of the join subobject.

$$
m(f \sqcup g \text{ factorisation}) = \operatorname{arrow}(f \sqcup g)
$$

Queries: “Why is the middle object of the image factorisation of the coproduct map equal to the join of subobjects?” · “(f \sqcup g).\mathrm{arrow} = m \text{ in the mono factorisation of } f \amalg g \to A” · “subobject join supMonoFactorisation arrow coproduct image lattice”

### `CategoryTheory.sheafToPresheafCompYonedaCompWhiskeringLeftSheafToPresheaf_hom_app_app_hom_apply_hom`

theorem in `Mathlib.CategoryTheory.Sites.Sheaf`

```lean
CategoryTheory.sheafToPresheafCompYonedaCompWhiskeringLeftSheafToPresheaf_hom_app_app_hom_apply_hom.{v₁, v₂, u₁, u₂}
  {C : Type u₁} [CategoryTheory.Category.{v₁, u₁} C] {J : CategoryTheory.GrothendieckTopology C} {A : Type u₂}
  [CategoryTheory.Category.{v₂, u₂} A] (X : CategoryTheory.Sheaf J A) (X✝ : (CategoryTheory.Sheaf J A)ᵒᵖ)
  (a✝ :
    (((CategoryTheory.sheafToPresheaf J A).comp
              (CategoryTheory.yoneda.comp
                ((CategoryTheory.Functor.whiskeringLeft (CategoryTheory.Sheaf J A)ᵒᵖ (CategoryTheory.Functor Cᵒᵖ A)ᵒᵖ
                      (Type (max u₁ v₂))).obj
                  (CategoryTheory.sheafToPresheaf J A).op))).obj
          X).obj
      X✝) :
  ((CategoryTheory.ConcreteCategory.hom
          ((CategoryTheory.sheafToPresheafCompYonedaCompWhiskeringLeftSheafToPresheaf.hom.app X).app X✝))
        a✝).hom =
    (Equiv.ulift.toIso.inv.hom' a✝).down
```

**Pointwise description of the Yoneda comparison isomorphism for sheaves**. This is a simp-normal-form lemma about the natural isomorphism `sheafToPresheafCompYonedaCompWhiskeringLeftSheafToPresheaf` relating the composite of the sheaf-to-presheaf inclusion, the Yoneda embedding and whiskering by the opposite of that inclusion to the plain Yoneda embedding. For a sheaf `X` on a site `(C, J)` with values in a category `A` and a sheaf `X'`, it computes the underlying function of the component at `X` and `X'` on an element `a` as the universe-lifted inverse of the identity, so that the result is the down-projection of `a`. Its proof is by unfolding the definition and simplifying with the universe-lifting equivalence. It is a bookkeeping identity for the Yoneda comparison rather than a substantive theorem about sheaves.

Queries: “Yoneda isomorphism for sheaves composed with sheafToPresheaf applied pointwise” · “sheafToPresheaf Yoneda whiskeringLeft hom apply simp lemma ULift” · “sheaf Yoneda embedding natural isomorphism component evaluation Equiv.ulift”

### `CategoryTheory.shrinkYonedaGrp_map_app_shrinkYonedaObjObjEquiv_symm`

theorem in `Mathlib.CategoryTheory.Monoidal.Cartesian.ShrinkYoneda`

```lean
CategoryTheory.shrinkYonedaGrp_map_app_shrinkYonedaObjObjEquiv_symm.{w, v, u} {C : Type u}
  [CategoryTheory.Category.{v, u} C] [CategoryTheory.LocallySmall.{w, v, u} C]
  [CategoryTheory.CartesianMonoidalCategory C] {M M' : CategoryTheory.Grp C} {Y : Cᵒᵖ} (f : Opposite.unop Y ⟶ M.X)
  (g : M ⟶ M') :
  (CategoryTheory.ConcreteCategory.hom ((CategoryTheory.shrinkYonedaGrp.{w, v, u}.map g).app Y))
      (CategoryTheory.shrinkYonedaGrpObjObjEquiv.symm f) =
    CategoryTheory.shrinkYonedaGrpObjObjEquiv.symm (CategoryTheory.CategoryStruct.comp f g.hom.hom)
```

**Naturality of the shrunken Yoneda group embedding on morphisms**. For a locally small Cartesian monoidal category $C$, group objects $M$ and $M'$ in $C$, an object $Y$ of $C^{\mathrm{op}}$, a morphism $f : \mathrm{unop}\, Y \to M.X$ and a group homomorphism $g : M \to M'$, applying the shrunken Yoneda map `CategoryTheory.shrinkYonedaGrp` of $g$ at $Y$ to the element corresponding to $f$ under `CategoryTheory.shrinkYonedaGrpObjObjEquiv` gives the element corresponding to the composite $f \circ g$. So the equivalence is natural in the group object variable.

$$
\mathrm{shrinkYonedaGrp}(g)_Y(\Phi^{-1}(f)) = \Phi^{-1}(f \circ g)
$$

Queries: “How does the shrunken Yoneda embedding act on morphisms of group objects in a Cartesian monoidal category?” · “shrinkYonedaGrp map app shrinkYonedaGrpObjObjEquiv symm naturality” · “Yoneda embedding group object composition g ∘ f locally small Cartesian monoidal category”

### `CochainComplex.mappingCocone.lift_f_snd_v`

theorem in `Mathlib.Algebra.Homology.HomotopyCategory.MappingCocone`

```lean
CochainComplex.mappingCocone.lift_f_snd_v.{v_1, u_1} {C : Type u_1} [CategoryTheory.Category.{v_1, u_1} C]
  [CategoryTheory.Preadditive C] {K L : CochainComplex C ℤ} (φ : K ⟶ L) [HomologicalComplex.HasHomotopyCofiber φ]
  {M : CochainComplex C ℤ} (α : M ⟶ K) (β : CochainComplex.HomComplex.Cochain M L (-1))
  (hαβ :
    CochainComplex.HomComplex.δ (-1) 0 β +
        CochainComplex.HomComplex.Cochain.ofHom (CategoryTheory.CategoryStruct.comp α φ) =
      0)
  (p q : ℤ) (hpq : p + -1 = q) :
  CategoryTheory.CategoryStruct.comp ((CochainComplex.mappingCocone.lift φ α β hαβ).f p)
      ((CochainComplex.mappingCocone.snd φ).v p q hpq) =
    β.v p q hpq
```

**Second component of the lift to the mapping cocone**. For a morphism $\varphi : K \to L$ of cochain complexes admitting a homotopy cofiber, a morphism $\alpha : M \to K$ and a degree $-1$ cochain $\beta$ from $M$ to $L$ satisfying $\delta\beta + \alpha \circ \varphi = 0$, composing the degree $p$ component of the lift of $(\alpha,\beta)$ to the mapping cocone with the second projection to $L$ gives $\beta$ in degrees $p$ and $q = p - 1$. This is the defining compatibility of `CochainComplex.mappingCocone.lift` with `CochainComplex.mappingCocone.snd`.

$$
\mathrm{lift}(\varphi,\alpha,\beta)_p \circ \mathrm{snd}(\varphi)_{p,q} = \beta_{p,q}
$$

Queries: “mapping cocone lift composed with second projection equals beta cochain” · “lift_f_snd_v mapping cocone homotopy cofiber cochain identity” · “\mathrm{snd} \circ \mathrm{lift}(\alpha,\beta) = \beta mapping cocone cochain complex” · “mapping cocone lift snd component cochain degree -1 keywords”

### `DerivedCategory.Plus.quotientCompQhIso`

def in `Mathlib.Algebra.Homology.DerivedCategory.Plus`

```lean
DerivedCategory.Plus.quotientCompQhIso.{v_1, u_1, u_2} (C : Type u_1) [CategoryTheory.Category.{v_1, u_1} C]
  [CategoryTheory.Abelian C] [HasDerivedCategory C] :
  (HomotopyCategory.Plus.quotient C).comp DerivedCategory.Plus.Qh ≅ DerivedCategory.Plus.Q
```

Docstring: The natural isomorphism `HomotopyCategory.Plus.quotient C ⋙ Qh ≅ Q`.

**Natural isomorphism between the bounded below homotopy-to-derived composite and the bounded below localization**. For an abelian category $\mathcal{C}$ with a derived category, this is the natural isomorphism $\mathrm{quotient} \circ Qh \cong Q$ between the composite of the quotient functor from bounded below cochain complexes to the bounded below homotopy category, followed by the localization $Qh$ to the bounded below derived category, and the localization functor $Q$ from bounded below cochain complexes to the bounded below derived category `DerivedCategory.Plus.Q`. It is the bounded-below analogue of `DerivedCategory.quotientCompQhIso`.

Queries: “natural isomorphism from bounded below homotopy category composed with derived localization to Q for bounded below complexes” · “isomorphism HomotopyCategory.Plus.quotient ⋙ Qh ≅ Q derived category” · “bounded below derived category localization functor natural iso quotient Qh Q keywords”

### `DirSupClosed.empty`

theorem in `Mathlib.Order.DirSupClosed`

```lean
DirSupClosed.empty.{u_1} {α : Type u_1} [Preorder α] : DirSupClosed ∅
```

**Empty set is closed under directed suprema**. The empty set is closed under directed suprema in any preorder $\alpha$: since no nonempty directed subset of $\alpha$ can lie inside $\varnothing$, the defining condition holds vacuously, as stated by `DirSupClosed`.

Queries: “is the empty set closed under directed suprema in a preorder” · “DirSupClosed ∅ preorder empty set lemma” · “empty set directed supremum closed vacuous”

### `EisensteinSeries.eisensteinSeriesSIF`

def in `Mathlib.NumberTheory.ModularForms.EisensteinSeries.Defs`

```lean
EisensteinSeries.eisensteinSeriesSIF {N : ℕ} (a : Fin 2 → ZMod N) (k : ℤ) :
  SlashInvariantForm (Subgroup.map (Matrix.SpecialLinearGroup.mapGL ℝ) (CongruenceSubgroup.Gamma N)) k
```

Docstring: The `SlashInvariantForm` defined by an Eisenstein series of weight `k : ℤ`, level `Γ(N)`, and congruence condition given by `a : Fin 2 → ZMod N`.

**Eisenstein series of level Γ(N) as a slash-invariant form**. This defines the Eisenstein series of weight $k \in \mathbb{Z}$ and level $\Gamma(N)$ attached to a congruence class $a : \text{Fin } 2 \to \mathbb{Z}/N\mathbb{Z}$, packaged as a `SlashInvariantForm` on the image of $\Gamma(N)$ in $\mathrm{GL}(2,\mathbb{R})$ under `Matrix.SpecialLinearGroup.mapGL`. Its underlying function is the sum over coprime integer pairs $(v_0, v_1)$ congruent to $a$ modulo $N$ of $(v_0 z + v_1)^{-k}$, so the form is invariant under the weight-$k$ slash action of this subgroup.

Aliases: Eisenstein series of level Γ(N)

Queries: “Eisenstein series of level N and weight k as a slash invariant form in Lean” · “\sum_{(c,d)\equiv a \bmod N,\ \gcd(c,d)=1} (cz+d)^{-k}” · “Eisenstein series congruence subgroup Gamma(N) weight k slash invariant form”

### `ExceptCpsT.run`

def in `Init.Control.ExceptCps`

```lean
ExceptCpsT.run.{u, u_1} {m : Type u → Type u_1} {ε α : Type u} [Monad m] (x : ExceptCpsT ε m α) : m (Except ε α)
```

Docstring: Use a monadic action that may throw an exception as an action that may return an exception's value.

**Running a CPS exception monad transformer**. Given a computation `x` in the continuation-passing-style exception transformer `ExceptCpsT ε m α` over a monad `m`, `ExceptCpsT.run` produces an action in `m` returning `Except ε α`, that is, either the thrown error of type `ε` or the successful value of type `α`. It converts a computation that may throw an exception into one that reports the exception's value as data.

Queries: “how to convert a monadic computation that may throw an exception into Except result in Lean” · “ExceptCpsT run function returning Except ε α in monad m” · “continuation passing style exception monad transformer run Except”

### `Fermion.rightDualRightToMatrix`

def in `Physlib.Relativity.Fermions.Weyl.Two`

```lean
Fermion.rightDualRightToMatrix :
  TensorProduct ℂ Fermion.RightHandedWeyl Fermion.DualRightHandedWeyl ≃ₗ[ℂ] Matrix (Fin 2) (Fin 2) ℂ
```

Docstring: Equivalence of `rightHanded ⊗ dualRightHanded` to `2 x 2` complex matrices.

**Equivalence between tensor product of right-handed and dual right-handed Weyl spinors and 2×2 matrices**. This is a $\mathbb{C}$-linear equivalence between the tensor product of a right-handed Weyl spinor $\psi^{\dot\alpha}$ and a dual right-handed Weyl spinor $\psi_{\dot\beta}$, and the space of $2 \times 2$ complex matrices. It identifies the tensor product of the two two-component spinor spaces with matrices indexed by $\mathrm{Fin}\,2$ × $\mathrm{Fin}\,2$.

$$
\mathbb{C}^2 \otimes \mathbb{C}^2 \cong M_{2}(\mathbb{C})
$$

Queries: “linear equivalence between tensor product of right-handed Weyl spinor and dual right-handed Weyl spinor and 2x2 complex matrices” · “\mathbb{C}^2 \otimes \mathbb{C}^2 \cong M_2(\mathbb{C}) Weyl spinor” · “Weyl fermion tensor product matrix equivalence right-handed dual SL(2,C)”

### `Int32.sub_mul`

theorem in `Init.Data.SInt.Lemmas`

```lean
Int32.sub_mul {a b c : Int32} : (a - b) * c = a * c - b * c
```

**Right distributivity of multiplication over subtraction for 32-bit integers**. For 32-bit signed integers $a$, $b$, $c$ (type `Int32`, arithmetic modulo $2^{32}$), multiplying a difference by $c$ gives the difference of the products: $(a - b) \cdot c = a \cdot c - b \cdot c$. This is the right distributive law of `Int32` multiplication over subtraction.

$$
(a - b) \cdot c = a \cdot c - b \cdot c
$$

Queries: “does multiplication distribute over subtraction for Int32 (a - b) * c = a * c - b * c” · “\((a - b) \cdot c = a \cdot c - b \cdot c\) 32-bit signed integer” · “Int32 sub mul distributivity right wraparound keywords”

### `InvariantBasisNumber.eq_of_fin_equiv`

theorem in `Mathlib.LinearAlgebra.InvariantBasisNumber`

```lean
InvariantBasisNumber.eq_of_fin_equiv.{u} {R : Type u} {inst✝ : Semiring R} [self : InvariantBasisNumber R] {n m : ℕ} :
  ∀ (a : (Fin n → R) ≃ₗ[R] Fin m → R), n = m
```

Docstring: Any linear equiv between `Rⁿ` and `Rᵐ` guarantees `m = n`.

**Invariant basis number: linear equivalence of free modules forces equal rank**. For a semiring $R$ with the invariant basis number property, any $R$-linear equivalence between $R^n$ and $R^m$ (functions from `Fin n` and `Fin m` to $R$) forces $n = m$. This is the defining property of `InvariantBasisNumber R`, so it makes the rank of a finitely generated free $R$-module well defined.

Aliases: Invariant basis number property; IBN property

Queries: “If there is a linear isomorphism between R^n and R^m, must n equal m for a semiring R?” · “invariant basis number linear equiv Fin n → R implies n = m” · “IBN ring free module rank well-defined isomorphism dimension”

### `List.toByteArray_append'`

theorem in `Init.Data.ByteArray.Bootstrap`

```lean
List.toByteArray_append' {l l' : List UInt8} : (l ++ l').toByteArray = l.toByteArray.append l'.toByteArray
```

**Byte array of a concatenation of byte lists**. For lists `l` and `l'` of bytes (`UInt8`), converting the concatenation `l ++ l'` to a `ByteArray` gives the same result as converting `l` and `l'` separately and appending the two byte arrays with `ByteArray.append`. This is the variant of `List.toByteArray_append` stated with the append written as `ByteArray.append`.

$$
(l \mathbin{++} l').\mathrm{toByteArray} = l.\mathrm{toByteArray}.\mathrm{append}\ l'.\mathrm{toByteArray}
$$

Queries: “how does List.toByteArray interact with list append for UInt8 lists” · “$(l ++ l').\mathrm{toByteArray} = l.\mathrm{toByteArray}.\mathrm{append}\, l'.\mathrm{toByteArray}$” · “List toByteArray append ByteArray UInt8 lemma”

Checks: {"latex":["\\mathbin isn't a KaTeX command"],"not_dependency":["List.toByteArray_append"]}

### `Lorentz.coContrContract_basis`

theorem in `Physlib.Relativity.Tensors.RealTensor.Vector.Pre.Contraction`

```lean
Lorentz.coContrContract_basis {d : ℕ} (i j : Fin 1 ⊕ Fin d) :
  Lorentz.coContrContract ((Lorentz.coBasis d) i ⊗ₜ[ℝ] (Lorentz.contrBasis d) j) = if i = j then 1 else 0
```

**Contraction of standard covariant and contravariant basis vectors**. For the standard basis of covariant Lorentz vectors $e_i$ and the standard basis of contravariant Lorentz vectors $e^j$ indexed by $i, j$ in $\mathrm{Fin}\,1 \oplus \mathrm{Fin}\,d$, the Lorentz-invariant contraction `Lorentz.coContrContract` of the tensor $e_i \otimes e^j$ equals $1$ if $i = j$ and $0$ otherwise. In other words, the contraction is the Kronecker delta on the standard basis.

$$
\langle e^{\mathrm{co}}_i, e^{\mathrm{contr}}_j \rangle = \delta_{ij}
$$

Queries: “What is the contraction of a covariant basis vector with a contravariant basis vector in Lorentz tensors?” · “\sum_\mu e_{\mu} e^{\nu} = \delta_{\mu\nu} contraction basis Lorentz” · “Lorentz contraction coBasis contrBasis kronecker delta”

### `MeasureTheory.OuterMeasure.coeFnAddMonoidHom_apply`

theorem in `Mathlib.MeasureTheory.OuterMeasure.Operations`

```lean
MeasureTheory.OuterMeasure.coeFnAddMonoidHom_apply.{u_1, u_2, u_3} {F : Type u_1} {α : Type u_2} {β : Type u_3}
  [FunLike F α β] [AddZero F] [AddZeroClass β] [IsZeroApply F α β] [IsAddApply F α β] (f : F) :
  (FunLike.coeAddMonoidHom F α β) f = ⇑f
```

Docstring: **Alias** of `FunLike.coeAddMonoidHom_apply`.

**Coercion of outer measures to functions as an additive monoid homomorphism**. For a type `F` of functions from `α` to `β` with zero and addition that act pointwise, applying the additive monoid homomorphism `FunLike.coeAddMonoidHom` to `f` gives the underlying function of `f`. This is the outer-measure version of the general `FunLike.coeAddMonoidHom_apply` lemma, so the coercion of an outer measure to a function is additive.

$$
\mathrm{coeAddMonoidHom}(f) = \Leftarrow f
$$

Queries: “coercion of outer measure to function as additive monoid hom applied to f equals coe f” · “FunLike.coeAddMonoidHom apply outer measure” · “outer measure coe additive monoid homomorphism lemma”

### `ModelWithCorners.MfldCat.coe_of`

theorem in `Mathlib.Geometry.Manifold.Category.MfldCat.OfModel`

```lean
ModelWithCorners.MfldCat.coe_of.{u, u_1, u_2, u_3} {𝕜 : Type u_1} [NontriviallyNormedField 𝕜] {E : Type u_2}
  [NormedAddCommGroup E] [NormedSpace 𝕜 E] {H : Type u_3} [TopologicalSpace H] (I : ModelWithCorners 𝕜 E H)
  {n : WithTop ℕ∞} (X : Type u) [TopologicalSpace X] [ChartedSpace H X] [IsManifold I n X] : ↑↧X = X
```

**Underlying type of a bundled manifold**. For a topological space $X$ that is a $C^n$ manifold modeled on the model with corners $I$, the underlying type of the bundled object `ModelWithCorners.MfldCat.of I n X` in the category of $C^n$ manifolds modeled on $I$ is definitionally $X$ itself, so its coercion to a type is $X$. This is a simp lemma restating the concrete-category coercion for the bundled manifold.

Queries: “underlying type of bundled manifold in the category of C^n manifolds modeled on I equals X” · “coe of MfldCat.of manifold is the manifold itself” · “ModelWithCorners MfldCat of coe simp lemma IsManifold”

### `Nat.lt_le_asymm`

theorem in `Init.Data.Nat.Basic`

```lean
Nat.lt_le_asymm {a b : ℕ} : a < b → ¬b ≤ a
```

**Strict inequality implies non-reverse inequality on natural numbers**. For natural numbers $a$ and $b$, if $a < b$ then it is not the case that $b \le a$. This is the asymmetry of the order combined with the trichotomy of strict and non-strict inequality, stated in the form useful for rewriting and contradiction proofs.

$$
a < b \implies \neg (b \le a)
$$

Queries: “if a < b then not b ≤ a for natural numbers” · “\(a < b \implies \neg (b \le a)\) Nat” · “Nat lt le asymmetry strict less than implies not greater or equal”

### `Opens.instIsThinPointOpensGrothendieckTopology`

theorem in `Mathlib.Topology.Sheaves.Points`

```lean
Opens.instIsThinPointOpensGrothendieckTopology.{u} {X : Type u} [TopologicalSpace X] :
  Quiver.IsThin (Opens.grothendieckTopology X).Point
```

**Points of the open-set site are thin**. For any topological space $X$, the quiver of points of the Grothendieck topology on the category of open subsets of $X$ is thin: between any two points there is at most one morphism. This is an instance, so any two morphisms between points of this site are equal.

Queries: “Is the category of points of the Grothendieck topology on opens of a topological space thin?” · “\text{Quiver.IsThin}(\text{Opens}(X).\text{Point})” · “instance thin quiver points Opens grothendieck topology topological space”

### `Option.forIn'_map`

theorem in `Init.Data.Option.Monadic`

```lean
Option.forIn'_map.{u_1, u_2, u_3, u_4} {m : Type u_1 → Type u_2} {α : Type u_3} {β : Type u_4} {γ : Type u_1} {init : γ}
  [Monad m] [LawfulMonad m] (o : Option α) (g : α → β) (f : (b : β) → b ∈ Option.map g o → γ → m (ForInStep γ)) :
  forIn' (Option.map g o) init f = forIn' o init fun a h y => f (g a) ⋯ y
```

**Monadic for-in loop over a mapped option**. For a monad m that is lawful, iterating with the monadic for-in loop `forIn'` over the option `Option.map g o` with initial state `init` and body `f` gives the same result as iterating over `o` directly, where the body applies `f` to `g a` (with the membership proof transported along the mapping). This is a fusion law showing that mapping an option before looping can be absorbed into the loop body.

$$
\mathrm{forIn}'\,(\mathrm{map}\,g\,o)\,\mathrm{init}\,f = \mathrm{forIn}'\,o\,\mathrm{init}\,(\lambda a\,h\,y.\, f\,(g\,a)\,\dots\,y)
$$

Queries: “forIn' over Option.map equals forIn' over original option with composed function” · “how does a monadic loop over a mapped option reduce to a loop over the option?” · “Option.map forIn' fusion lemma LawfulMonad”

### `PULift`

structure in `Init.Prelude`

```lean
PULift.{r, s} (α : Sort s) : Sort (max s r 1)
```

Docstring: Lifts a type or proposition to a higher universe level. `PULift α` wraps a value of type `α`. It is a generalization of `PLift` that allows lifting values whose type may live in `Sort s`. It also subsumes `PLift`.

**Universe-polymorphic lift structure**. `PULift α` is a structure wrapping a single value of type `α : Sort s` in a universe `Sort (max s r 1)`, where `r` is a free universe parameter. Its constructor `PULift.up` builds an element from a value of `α`, and `PULift.down` recovers the value, so that the type is moved to a higher universe level. It generalizes `PLift` from propositions to arbitrary sorts and includes `PLift` as a special case.

Queries: “how to lift a type to a higher universe level in Lean” · “PULift structure universe polymorphic wrapper up down” · “lift Sort s to Sort (max s r 1) universe lifting”

Checks: {"not_dependency":["PULift.up","PULift.down"]}

### `Physlib.Fin.finExtractTwo_apply_snd`

theorem in `Physlib.Mathematics.ForMathlib.Fin`

```lean
Physlib.Fin.finExtractTwo_apply_snd {n : ℕ} (i : Fin n.succ.succ) (j : Fin n.succ) :
  (Physlib.Fin.finExtractTwo i j) (i.succAbove j) = Sum.inl (Sum.inr 0)
```

**Second extracted index of the two-index Fin splitting**. For $i \in \mathrm{Fin}(n+2)$ and $j \in \mathrm{Fin}(n+1)$, the equivalence `Physlib.Fin.finExtractTwo` sends the index $i$.succAbove $j$ to the second copy of $\mathrm{Fin}\,1$ in the first summand, that is, to `Sum.inl (Sum.inr 0)`. This is the statement that the element $i$.succAbove $j$ is the one extracted as the second of the two special indices.

$$
\mathrm{finExtractTwo}(i)(i.\mathrm{succAbove}\ j) = \mathrm{inl}(\mathrm{inr}\ 0)
$$

Queries: “finExtractTwo applied to succAbove sends index to second summand Sum.inl (Sum.inr 0)” · “Fin (n+2) equivalence extracting two indices, succAbove maps to inl inr 0” · “Physlib Fin finExtractTwo_apply_snd lemma”

### `Quiver.SchreierGraph.labellingStarEquiv_symm_apply_fst`

theorem in `Mathlib.Combinatorics.Quiver.Schreier`

```lean
Quiver.SchreierGraph.labellingStarEquiv_symm_apply_fst.{u_1, u_2, u_3} {V : Type u_1} {M : Type u_2} [Group M]
  [MulAction M V] {S : Type u_3} (ι : S → M) (x : Quiver.SchreierGraph V ι)
  (x✝ : Quiver.Star (Quiver.SingleObj.star S)) :
  ((Quiver.SchreierGraph.labellingStarEquiv ι x).symm x✝).fst = ι x✝.snd • x
```

**Source vertex of the inverse star labelling in a Schreier graph**. For a Schreier graph of a group $M$ acting on $V$ with generators $\iota : S \to M$, and a vertex $x$, the inverse of the star equivalence `Quiver.SchreierGraph.labellingStarEquiv` sends an outgoing edge labelled by $s \in S$ to a Schreier edge whose target vertex is $\iota(s) \cdot x$. In other words, the first component of the edge is the vertex $\iota(s) \cdot x$ reached from $x$ by acting with the generator labelled $s$.

$$
\pi_1\big(\Phi_x^{-1}(s)\big) = \iota(s)\cdot x
$$

Queries: “Schreier graph edge from x labelled s points to ι(s) acting on x” · “labellingStarEquiv symm apply fst group action generator vertex” · “Schreier graph outgoing edge target is generator times vertex group action”

### `Rat.lt_iff_le_and_not_ge`

theorem in `Init.Data.Rat.Lemmas`

```lean
Rat.lt_iff_le_and_not_ge {a b : ℚ} : a < b ↔ a ≤ b ∧ ¬b ≤ a
```

**Strict order on the rationals as non-strict order and non-reverse**. For rational numbers $a$ and $b$, $a < b$ holds if and only if $a \le b$ and not $b \le a$. This is the standard characterization of the strict order in terms of the non-strict order, stated for the rationals.

$$
a < b \iff a \le b \land \neg (b \le a)
$$

Queries: “how to show a < b for rationals from a ≤ b and not b ≤ a” · “a < b ↔ a ≤ b ∧ ¬ b ≤ a rational numbers” · “Rat strict less than iff less or equal and not greater or equal”

### `Rat.mul_inv_cancel`

theorem in `Init.Data.Rat.Lemmas`

```lean
Rat.mul_inv_cancel (a : ℚ) : a ≠ 0 → a * a⁻¹ = 1
```

**Multiplicative inverse of a nonzero rational number**. For every nonzero rational number $a$, the product of $a$ with its multiplicative inverse $a^{-1}$ equals $1$. This is the field axiom for the rationals, stated for the core Lean library's `Rat` type.

$$
a \neq 0 \implies a \cdot a^{-1} = 1
$$

Queries: “Why is a times its inverse equal to 1 for a nonzero rational number?” · “a \neq 0 \implies a \cdot a^{-1} = 1 for rationals” · “rational number inverse cancel nonzero multiplication field”

### `Space.div_eq_sum_fderiv`

theorem in `Physlib.SpaceAndTime.Space.Derivatives.Div`

```lean
Space.div_eq_sum_fderiv {d : ℕ} (f : Space d → EuclideanSpace ℝ (Fin d)) (hf : Differentiable ℝ f) (x : Space d) :
  Space.div f x = ∑ i, ((fderiv ℝ f x) (Space.basis i)).ofLp i
```

**Divergence as the sum of partial derivatives**. For a differentiable vector field $f$ from `Space d` to `EuclideanSpace ℝ (Fin d)`, the divergence `Space.div f` at a point $x$ equals the sum over coordinate directions $i$ of the $i$-th component of the Fréchet derivative of $f$ at $x$ applied to the standard basis vector `Space.basis i`. This is the usual formula $\nabla \cdot f = \sum_i \partial_i f_i$ unfolded from the definition of `Space.div`.

$$
\nabla \cdot f(x) = \sum_{i} \partial_i f_i(x)
$$

Queries: “divergence of a differentiable vector field equals sum of partial derivatives of components fderiv” · “\nabla \cdot f(x) = \sum_i \partial_i f_i(x)” · “Space.div fderiv basis sum divergence formula”

### `Std.DTreeMap.Internal.Impl.insertIfNew`

def in `Std.Data.DTreeMap.Internal.Operations`

```lean
Std.DTreeMap.Internal.Impl.insertIfNew.{u, v} {α : Type u} {β : α → Type v} [Ord α] (k : α) (v : β k)
  (t : Std.DTreeMap.Internal.Impl α β) (hl : t.Balanced) :
  Std.DTreeMap.Internal.Impl.SizedBalancedTree α β t.size (t.size + 1)
```

Docstring: Adds a new mapping to the tree, leaving the tree unchanged if the key is already present.

**Insert a new key into a balanced tree if absent**. Given a key $k$, a value $v$ and a balanced tree $t$ of size $n$, this returns a size-bounded balanced tree whose size is $n$ or $n+1$ and that has size bounded by $n+1$, namely the tree with $(k,v)$ inserted when $k$ is not already a key, and $t$ unchanged when it is. The `Std.DTreeMap.Internal.Impl` tree is the internal implementation of the ordered dependent map `Std.DTreeMap`, ordered by the `Ord` instance on keys.

Queries: “how to insert a key into a balanced binary search tree only if it is not already present” · “Lean DTreeMap insertIfNew implementation balanced tree size bound” · “insert if absent ordered map tree size t.size + 1 keys Ord”

### `Std.HashMap.Raw.getKey!_alter_self`

theorem in `Std.Data.HashMap.RawLemmas`

```lean
Std.HashMap.Raw.getKey!_alter_self.{u, v} {α : Type u} {β : Type v} [BEq α] [Hashable α] {m : Std.HashMap.Raw α β}
  [EquivBEq α] [LawfulHashable α] [Inhabited α] {k : α} {f : Option β → Option β} (h : m.WF) :
  (m.alter k f).getKey! k = if (f m[k]?).isSome = true then k else default
```

**Key lookup after altering a hash map at its own key**. For a well-formed raw hash map `m` with keys of type `α` (under `EquivBEq α`, `LawfulHashable α`, `Inhabited α`), altering the entry at key `k` by a function `f : Option β → Option β` and then looking up the stored key equal to `k` returns `k` if `f` applied to the current value `m[k]?` is `some`, and returns `default` otherwise. This is the key-retrieval counterpart of the value lemma for `Std.HashMap.Raw.alter`.

$$
\mathrm{getKey}!_{\mathrm{alter}\,k\,f}(k) = \begin{cases} k & \text{if } f(m[k]?) \text{ is some} \\ \mathrm{default} & \text{otherwise} \end{cases}
$$

Queries: “what key does getKey! return after alter on the same key in a hash map” · “\mathrm{getKey}!(\mathrm{alter}\, m\, k\, f)(k) = k \text{ if } f(m[k]?) \text{ is some}” · “HashMap Raw getKey! alter self lemma default inhabited keys”

### `Std.IsLinearOrder.of_le`

theorem in `Init.Data.Order.Factories`

```lean
Std.IsLinearOrder.of_le.{u} {α : Type u} [LE α]
  (le_antisymm : Std.Antisymm fun x1 x2 => x1 ≤ x2 := by exact inferInstance)
  (le_trans : Trans (fun x1 x2 => x1 ≤ x2) (fun x1 x2 => x1 ≤ x2) fun x1 x2 => x1 ≤ x2 := by exact inferInstance)
  (le_total : Std.Total fun x1 x2 => x1 ≤ x2 := by exact inferInstance) : Std.IsLinearOrder α
```

Docstring: If an `LE α` instance is antisymmetric, transitive and total, then it represents a linear order.

**Linear order from antisymmetric, transitive, total less-equal**. If a relation $\le$ on a type $\alpha$ is antisymmetric, transitive and total, then it makes $\alpha$ a linear order in the sense of the `Std.IsLinearOrder` typeclass. This is a factory lemma that builds the `Std.IsLinearOrder` instance from the three separate properties of `≤`, so that they need not be supplied as a bundled structure.

Queries: “how to show a less-equal relation is a linear order from antisymmetry transitivity and totality” · “Std.IsLinearOrder instance from le_antisymm le_trans le_total” · “linear order factory antisymmetric transitive total LE instance”

### `Std.Net.MACAddr`

structure in `Std.Net.Addr`

```lean
Std.Net.MACAddr : Type
```

Docstring: Representation of a MAC address.

**MAC address structure**. A structure representing a MAC (Media Access Control) address, the 48-bit hardware identifier assigned to a network interface. It belongs to the networking address types in Lean's standard library, alongside IPv4 and IPv6 addresses and socket addresses.

Aliases: Ethernet hardware address; MAC-48 address

Queries: “what type represents a MAC address in Lean networking” · “48-bit hardware address Ethernet structure Std.Net” · “MAC address network interface identifier type”

### `Std.Sat.AIG.of_isConstant`

theorem in `Std.Sat.AIG.Lemmas`

```lean
Std.Sat.AIG.of_isConstant {α : Type} [Hashable α] [DecidableEq α] {aig : Std.Sat.AIG α} {assign : α → Bool}
  {ref : aig.Ref} {b : Bool} : aig.isConstant ref b = true → ⟦assign, { aig := aig, ref := ref }⟧ = b
```

**Evaluation of a constant AIG reference**. If the and-inverter graph `aig` reports that the reference `ref` is constant with value `b` via `Std.Sat.AIG.isConstant`, then evaluating `ref` under any assignment `assign` of the variables yields `b`. Here `assign : α → Bool` assigns Boolean values to the input variables of the AIG.

$$
\mathrm{isConstant}(\mathrm{ref}) = b \implies \llbracket \mathrm{assign}, \mathrm{ref} \rrbracket = b
$$

Queries: “If an AIG node is detected as constant true or false, does it evaluate to that constant under every assignment?” · “isConstant ref b = true → eval assign ref = b AIG SAT” · “and-inverter graph constant reference soundness Bool assignment”

Checks: {"latex":["\\llbracket isn't a KaTeX command","\\rrbracket isn't a KaTeX command"]}

### `String.utf8EncodeChar_eq_cons_cons_cons_cons`

theorem in `Init.Data.String.Decode`

```lean
String.utf8EncodeChar_eq_cons_cons_cons_cons {c : Char} :
  c.utf8Size = 4 →
    String.utf8EncodeChar c =
      [(c.val >>> 18).toUInt8 &&& 7 ||| 240, (c.val >>> 12).toUInt8 &&& 63 ||| 128,
        (c.val >>> 6).toUInt8 &&& 63 ||| 128, c.val.toUInt8 &&& 63 ||| 128]
```

**UTF-8 encoding of a four-byte character**. For a character $c$ whose UTF-8 size is $4$, the UTF-8 encoding of $c$ as a list of bytes is the list of four bytes formed from the code point of $c$: the first byte is $(c \gg 18)$ masked with $7$ and OR-ed with $240$, the second is $(c \gg 12)$ masked with $63$ and OR-ed with $128$, the third is $(c \gg 6)$ masked with $63$ and OR-ed with $128$, and the fourth is the low six bits of $c$ OR-ed with $128$. This is the low-level byte formula for four-byte UTF-8 encoding used in the proof that UTF-8 decoding and encoding are inverses.

$$
\mathrm{utf8EncodeChar}(c) = [(c \gg 18) \,\&\, 7 \mid 240,\ (c \gg 12) \,\&\, 63 \mid 128,\ (c \gg 6) \,\&\, 63 \mid 128,\ c \,\&\, 63 \mid 128] \quad\text{if } \mathrm{utf8Size}(c)=4
$$

Queries: “how is a Unicode character with four UTF-8 bytes encoded as bytes in Lean” · “utf8EncodeChar size 4 byte formula shifts masks 240 128” · “UTF-8 four byte encoding code point bit shifts 18 12 6”

### `TopCat.toSSet_map_const`

theorem in `Mathlib.AlgebraicTopology.SimplicialSet.TopAdj`

```lean
TopCat.toSSet_map_const.{u} (X : TopCat) {Y : TopCat} (y : ↑Y) :
  TopCat.toSSet.map (TopCat.const y) = SSet.const (TopCat.toSSetObj₀Equiv.symm y)
```

**Geometric realization-singular adjunction sends constant maps to constant simplicial maps**. For a topological space $X$ and a topological space $Y$ with a point $y \in Y$, applying the singular simplicial set functor `TopCat.toSSet` to the constant continuous map `TopCat.const y` from $X$ to $Y$ gives the constant morphism of simplicial sets `SSet.const` at the 0-simplex corresponding to $y$ under the equivalence `TopCat.toSSetObj₀Equiv`.

$$
\mathrm{toSSet}(\mathrm{const}_y) = \mathrm{const}_{y}
$$

Queries: “singular simplicial set of a constant map is constant simplicial map” · “toSSet map const equals SSet.const of point” · “TopCat.toSSet constant continuous map simplicial set $\mathrm{const}$ functor”

### `UInt64.mul_one`

theorem in `Init.Data.UInt.Lemmas`

```lean
UInt64.mul_one (a : UInt64) : a * 1 = a
```

**Right identity of multiplication on 64-bit unsigned integers**. For every 64-bit unsigned integer $a$, multiplying $a$ by the numeral $1$ gives $a$ back, so $1$ is a right identity for multiplication in `UInt64`. The multiplication is modulo $2^{64}$.

$$
a \cdot 1 = a
$$

Queries: “Does multiplying a UInt64 by 1 return the same value?” · “a \cdot 1 = a for 64-bit unsigned integers” · “UInt64 mul one identity right unit modular multiplication”

### `dimH_orthogonalProjectionOnto_le`

theorem in `Mathlib.Topology.MetricSpace.HausdorffDimension`

```lean
dimH_orthogonalProjectionOnto_le.{u_4, u_5} {𝕜 : Type u_4} {E : Type u_5} [RCLike 𝕜] [NormedAddCommGroup E]
  [InnerProductSpace 𝕜 E] (K : Submodule 𝕜 E) [K.HasOrthogonalProjection] (s : Set E) :
  dimH (⇑K.orthogonalProjectionOnto '' s) ≤ dimH s
```

Docstring: The Hausdorff dimension of the orthogonal projection of a set `s` onto a subspace `K` is less than or equal to the Hausdorff dimension of `s`.

**Hausdorff dimension does not increase under orthogonal projection**. For a subspace $K$ of an inner product space $E$ over $\mathbb{K}$ (real or complex) admitting an orthogonal projection, and any set $s \subseteq E$, the Hausdorff dimension of the image of $s$ under the orthogonal projection onto $K$ is at most the Hausdorff dimension of $s$. This is the statement that orthogonal projections are 1-Lipschitz, so they cannot increase Hausdorff dimension.

$$
\dim_H\big(P_K(s)\big) \le \dim_H(s)
$$

Queries: “Does orthogonal projection onto a subspace increase Hausdorff dimension of a set?” · “\dim_H(P_K(s)) \le \dim_H(s) for orthogonal projection P_K” · “Hausdorff dimension projection inner product space subspace image set bound”

### `isMinOn_Iic_of_deriv`

theorem in `Mathlib.Analysis.Calculus.DerivativeTest`

```lean
isMinOn_Iic_of_deriv {f : ℝ → ℝ} {b c : ℝ} (hb : ContinuousAt f b) (hc : ContinuousAt f c)
  (hd₀ : DifferentiableOn ℝ f (Set.Iio b)) (hd₁ : DifferentiableOn ℝ f (Set.Ioo b c))
  (h₀ : ∀ x ∈ Set.Iio b, deriv f x ≤ 0) (h₁ : ∀ x ∈ Set.Ioo b c, 0 ≤ deriv f x) : IsMinOn f (Set.Iic c) b
```

Docstring: Suppose `f : ℝ → ℝ` is continuous at `b` and `c`, the derivative `f'` is nonpositive on `Iio b` and nonnegative on `Ioo b c`. Then `f` attains its minimum on `Iic c` at `b`.

**First-derivative test for a minimum on a half-line**. Let $f:\mathbb{R}\to\mathbb{R}$ be continuous at $b$ and $c$, differentiable on $(-\infty,b)$ and on $(b,c)$, with $f'(x)\le 0$ for $x<b$ and $f'(x)\ge 0$ for $b<x<c$. Then $f$ attains its minimum over $(-\infty,c]$ at $b$, so $f(b)\le f(x)$ for all $x\le c$. This is the first-derivative test in its strong form.

$$
f(b)\le f(x)\quad\text{for all }x\le c
$$

Aliases: First-derivative test

Queries: “first derivative test minimum at b when derivative is nonpositive before b and nonnegative after b” · “f(b) \le f(x) for all x \le c given f' \le 0 on (-\infty,b) and f' \ge 0 on (b,c)” · “first derivative test local minimum real function continuous at endpoints deriv sign change IsMinOn Iic”

### `zorn_ge_nonempty_Iic₀`

theorem in `Mathlib.Order.Zorn`

```lean
zorn_ge_nonempty_Iic₀.{u_1} {α : Type u_1} [Preorder α] (a : α)
  (ih : ∀ c ⊆ Set.Iic a, IsChain (fun x1 x2 => x2 ≤ x1) c → ∀ y ∈ c, ∃ ub, ∀ z ∈ c, ub ≤ z) (x : α) (hax : x ≤ a) :
  ∃ m ≤ x, IsMin m
```

**Existence of a minimal element below a given element via Zorn's lemma**. Let $\alpha$ be a preorder and $a \in \alpha$. If every chain $c$ contained in $\{y \mid y \le a\}$ that is totally ordered by the reverse relation and has an element $y \in c$ admits a lower bound in $c$, then for every $x \le a$ there is a minimal element $m \le x$. This is the order-dual form of Zorn's lemma, restricted to the downward set below $a$, with the conclusion that $m$ is no larger than the given $x$.

$$
\forall x \le a,\ \exists m \le x,\ \forall b,\ b \le m \to m \le b
$$

Aliases: Zorn's lemma

Queries: “Zorn's lemma: every element below a bound has a minimal element below it when chains have lower bounds” · “\exists m \le x, \forall b \le m, m \le b” · “Zorn lemma minimal element Iic chain lower bound preorder”

