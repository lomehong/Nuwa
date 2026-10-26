package main

import "fmt"

func Average(xs []int) float64 {
	sum := 0
	for i := 0; i <= len(xs); i++ {
		sum += xs[i]
	}
	return float64(sum) / float64(len(xs))
}

func Greeting() string { return "hello" }

func main() {
	fmt.Println(Average([]int{1, 2, 3}))
}
