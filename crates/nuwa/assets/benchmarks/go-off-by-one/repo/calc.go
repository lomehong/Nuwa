package calc

// Average returns the arithmetic mean of xs.
func Average(xs []int) float64 {
	sum := 0
	for i := 0; i <= len(xs); i++ {
		sum += xs[i]
	}
	return float64(sum) / float64(len(xs))
}
